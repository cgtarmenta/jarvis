//! Spec data model + frontmatter parser.
//!
//! We *do not* pull in a full YAML crate (~300 KB to the binary). The
//! frontmatter we accept is a deliberately small subset — flat `key: value`
//! and `key:\n  - item\n  - item` lists — that we can parse with a tiny
//! hand-rolled state machine. That subset is documented in
//! `specs/_template.md` so users know what's supported.
//!
//! Anything outside the subset is preserved verbatim in `extra` so we never
//! silently drop user-added keys (forward-compat with future fields).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

/// Where a spec lives in the on-disk hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Inbox,
    Active,
    Shipped,
    Rejected,
    Private,
}

impl Status {
    pub fn dir(self) -> &'static str {
        match self {
            Status::Inbox => "inbox",
            Status::Active => "active",
            Status::Shipped => "shipped",
            Status::Rejected => "rejected",
            Status::Private => "private",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "inbox" => Some(Status::Inbox),
            "active" => Some(Status::Active),
            "shipped" => Some(Status::Shipped),
            "rejected" => Some(Status::Rejected),
            "private" => Some(Status::Private),
            _ => None,
        }
    }
}

/// The structured fields we recognise. Anything else lands in `extra` so
/// users can experiment with new keys without us losing them on rewrite.
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    /// Numeric ID for promoted specs. `None` for inbox/private entries.
    pub id: Option<u32>,
    pub title: String,
    pub status: Option<Status>,
    pub owner: String,
    pub created: String,
    pub shipped: Option<String>,
    pub verifying: Vec<String>,
    pub related: Vec<u32>,
    /// Unrecognised keys preserved verbatim.
    pub extra: BTreeMap<String, String>,
}

/// A spec loaded from disk.
#[derive(Debug, Clone)]
pub struct Spec {
    pub frontmatter: Frontmatter,
    /// Markdown body after the frontmatter block. We do not parse this —
    /// the wizard just writes it back wholesale when promoting / shipping.
    pub body: String,
    /// Absolute path on disk.
    pub path: PathBuf,
}

impl Spec {
    pub fn parse(raw: &str, path: &Path) -> Result<Self> {
        let (front_text, body) = split_frontmatter(raw)?;
        let frontmatter = parse_frontmatter(front_text)?;
        Ok(Self {
            frontmatter,
            body: body.to_string(),
            path: path.to_path_buf(),
        })
    }

    /// Render back to the on-disk format. Frontmatter keys are emitted in
    /// a stable order so diffs stay clean across edits.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        write_field(
            &mut out,
            "id",
            self.frontmatter.id.map(|n| format!("{n:04}")).as_deref(),
        );
        write_field(&mut out, "title", Some(&self.frontmatter.title));
        write_field(&mut out, "status", self.frontmatter.status.map(|s| s.dir()));
        write_field(&mut out, "owner", Some(&self.frontmatter.owner));
        write_field(&mut out, "created", Some(&self.frontmatter.created));
        write_field(&mut out, "shipped", self.frontmatter.shipped.as_deref());
        write_list(&mut out, "verifying", &self.frontmatter.verifying);
        let related: Vec<String> = self
            .frontmatter
            .related
            .iter()
            .map(|n| format!("{n:04}"))
            .collect();
        write_list(&mut out, "related", &related);
        // Preserve unknown keys verbatim — never silently drop user data.
        for (k, v) in &self.frontmatter.extra {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }

    /// True if every checkbox in the `## What` section is checked. Used by
    /// `spec ship` to gate the move from active/ → shipped/.
    pub fn what_all_checked(&self) -> bool {
        let mut in_what = false;
        for line in self.body.lines() {
            let trimmed = line.trim_start();
            if let Some(heading) = trimmed.strip_prefix("## ") {
                in_what = heading.trim().eq_ignore_ascii_case("What");
                continue;
            }
            if !in_what {
                continue;
            }
            // A bullet that opens with `[ ]` is an open acceptance criterion.
            if trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]") {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

fn split_frontmatter(raw: &str) -> Result<(&str, &str)> {
    let stripped = raw
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("spec must start with `---\\n` frontmatter delimiter"))?;
    // The closing fence is `\n---\n` or `\n---` at end of file.
    let (front, rest) = if let Some(pos) = stripped.find("\n---\n") {
        (&stripped[..pos], &stripped[pos + 5..])
    } else if let Some(pos) = stripped.find("\n---") {
        (&stripped[..pos], &stripped[pos + 4..])
    } else {
        bail!("spec frontmatter is missing closing `---` fence");
    };
    Ok((front, rest))
}

fn parse_frontmatter(text: &str) -> Result<Frontmatter> {
    let mut fm = Frontmatter::default();
    let mut iter = text.lines().peekable();

    while let Some(line) = iter.next() {
        if line.trim().is_empty() {
            continue;
        }
        // Trailing comments after `#` are stripped for value-only lines —
        // not for list items (which often contain `#` themselves in URLs).
        let (key, value) = match line.split_once(':') {
            Some(kv) => kv,
            None => continue, // Skip malformed lines rather than failing the whole parse.
        };
        let key = key.trim();
        let value = value.trim();

        if value.is_empty() {
            // Either an empty value (`shipped:`) or the start of a list.
            // Peek ahead: if the next non-blank line begins with whitespace
            // + `-`, it's a list; otherwise it's an empty scalar.
            let mut items = Vec::new();
            while let Some(peek) = iter.peek() {
                let trimmed = peek.trim_start();
                if trimmed.starts_with("- ") || trimmed.starts_with("-\t") {
                    items.push(trimmed[2..].trim().to_string());
                    iter.next();
                } else if trimmed.is_empty() {
                    iter.next();
                    continue;
                } else if !peek.starts_with(' ') && !peek.starts_with('\t') {
                    break;
                } else {
                    // Indented but not a list item — bail out of list parse.
                    break;
                }
            }
            assign_list(&mut fm, key, items);
        } else {
            assign_scalar(&mut fm, key, value);
        }
    }
    Ok(fm)
}

fn assign_scalar(fm: &mut Frontmatter, key: &str, value: &str) {
    match key {
        "id" => {
            // Accept both `14` and `0014`.
            if let Ok(n) = value.parse::<u32>() {
                fm.id = Some(n);
            }
        }
        "title" => fm.title = value.to_string(),
        "status" => fm.status = Status::parse(value),
        "owner" => fm.owner = value.to_string(),
        "created" => fm.created = value.to_string(),
        "shipped" => {
            if !value.is_empty() {
                fm.shipped = Some(value.to_string());
            }
        }
        other => {
            fm.extra.insert(other.to_string(), value.to_string());
        }
    }
}

fn assign_list(fm: &mut Frontmatter, key: &str, items: Vec<String>) {
    match key {
        "verifying" => fm.verifying = items,
        "related" => {
            // Each item is either a bare integer or "0014  # comment".
            fm.related = items
                .into_iter()
                .filter_map(|s| {
                    let id_part = s.split_whitespace().next()?;
                    id_part.parse::<u32>().ok()
                })
                .collect();
        }
        other => {
            // Stash multi-line lists as a single space-joined string in
            // `extra`. Lossy but better than dropping.
            fm.extra.insert(other.to_string(), items.join(", "));
        }
    }
}

fn write_field(out: &mut String, key: &str, value: Option<&str>) {
    if let Some(v) = value
        && !v.is_empty()
    {
        out.push_str(&format!("{key}: {v}\n"));
    } else if matches!(key, "id" | "shipped") {
        // Always emit these keys (even when empty) so the schema is stable
        // and the user can fill them in by hand if they want.
        out.push_str(&format!("{key}:\n"));
    } else {
        out.push_str(&format!("{key}:\n"));
    }
}

fn write_list(out: &mut String, key: &str, items: &[String]) {
    if items.is_empty() {
        out.push_str(&format!("{key}:\n"));
        return;
    }
    out.push_str(&format!("{key}:\n"));
    for item in items {
        out.push_str(&format!("  - {item}\n"));
    }
}

// ---------------------------------------------------------------------------
// Slug + filename helpers
// ---------------------------------------------------------------------------

/// Normalise a free-text title into a filesystem-safe kebab-case slug.
/// Drops accents, lowercases, replaces whitespace runs with `-`, caps at
/// 40 chars so filenames stay readable in `ls`.
pub fn slugify(title: &str) -> String {
    let stripped: String = title
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let slug: String = stripped.split_whitespace().collect::<Vec<_>>().join("-");
    slug.chars()
        .take(40)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// `inbox/2026-05-13-streaming-tts.md`
pub fn inbox_filename(date: &str, slug: &str) -> String {
    format!("{date}-{slug}.md")
}

/// `active/0014-streaming-tts.md` / same shape for shipped/rejected.
pub fn numbered_filename(id: u32, slug: &str) -> String {
    format!("{id:04}-{slug}.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\n\
id: 0014\n\
title: Pluggable wake backends\n\
status: active\n\
owner: tadeo\n\
created: 2026-05-13\n\
verifying:\n\
  - tests/wake_integration.rs::pluggable_backend_smoke\n\
  - cargo run -- test-wake --threshold 0.02\n\
related:\n\
  - 0011\n\
  - 0003  # session\n\
---\n\
# Title\n\n## Why\n\nbody...\n";

    #[test]
    fn parses_full_frontmatter() {
        let s = Spec::parse(SAMPLE, Path::new("/tmp/x.md")).unwrap();
        assert_eq!(s.frontmatter.id, Some(14));
        assert_eq!(s.frontmatter.title, "Pluggable wake backends");
        assert_eq!(s.frontmatter.status, Some(Status::Active));
        assert_eq!(s.frontmatter.verifying.len(), 2);
        assert_eq!(s.frontmatter.related, vec![11, 3]);
        assert!(s.body.starts_with("# Title"));
    }

    #[test]
    fn serialize_roundtrips_known_fields() {
        let s = Spec::parse(SAMPLE, Path::new("/tmp/x.md")).unwrap();
        let out = s.serialize();
        let s2 = Spec::parse(&out, Path::new("/tmp/x.md")).unwrap();
        assert_eq!(s.frontmatter.id, s2.frontmatter.id);
        assert_eq!(s.frontmatter.title, s2.frontmatter.title);
        assert_eq!(s.frontmatter.verifying, s2.frontmatter.verifying);
        assert_eq!(s.frontmatter.related, s2.frontmatter.related);
    }

    #[test]
    fn slugify_strips_accents_and_limits_length() {
        assert_eq!(slugify("Hola Mutombo"), "hola-mutombo");
        assert_eq!(slugify("Pingüino Más Rápido!"), "pinguino-mas-rapido");
        let long = slugify(&"x".repeat(100));
        assert!(long.len() <= 40);
    }

    #[test]
    fn what_all_checked_detects_open_criteria() {
        let body = "## What\n\n- [x] one\n- [ ] two\n- [x] three\n";
        let s = Spec {
            frontmatter: Frontmatter::default(),
            body: body.to_string(),
            path: PathBuf::new(),
        };
        assert!(!s.what_all_checked());

        let body_done = "## What\n\n- [x] one\n- [x] two\n";
        let s = Spec {
            frontmatter: Frontmatter::default(),
            body: body_done.to_string(),
            path: PathBuf::new(),
        };
        assert!(s.what_all_checked());
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        let raw = "# no frontmatter here\n";
        assert!(Spec::parse(raw, Path::new("/tmp/x.md")).is_err());
    }
}
