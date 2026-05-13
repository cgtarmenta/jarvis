//! Filesystem operations for the `specs/` directory.
//!
//! Spec management is intentionally implemented in pure deterministic Rust,
//! never delegated to the agent. The agent gets to write *content* (Why /
//! What / How) but never decides what file goes where. That separation keeps
//! the system testable and predictable: a unit test can rename a file from
//! `inbox/foo.md` to `active/0014-foo.md` without spawning anything.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use super::spec::{Frontmatter, Spec, Status, inbox_filename, numbered_filename, slugify};

/// Find the project's `specs/` directory by walking up from `start`. We
/// don't assume the user runs `jarvis spec` from the repo root — they
/// might be deep in `src/`, `tests/`, or anywhere else.
pub fn find_specs_dir(start: &Path) -> Result<PathBuf> {
    let mut cur: PathBuf = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        let candidate = cur.join("specs");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if !cur.pop() {
            bail!(
                "no `specs/` directory found in any parent of {}; run `jarvis spec` from \
                 inside a jarvis-style repository, or create `specs/` first",
                start.display()
            );
        }
    }
}

/// Convenience that finds `specs/` from the current working directory.
pub fn find_specs_dir_from_cwd() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("getting current directory")?;
    find_specs_dir(&cwd)
}

/// Load every parseable spec from every status subdirectory.
///
/// Files that fail to parse are *silently skipped* — we don't want a stray
/// editor backup file or a half-written WIP to break `jarvis spec list`. A
/// future `jarvis spec lint` could surface the failures explicitly.
pub fn list_all(specs_dir: &Path) -> Result<Vec<Spec>> {
    let mut out = Vec::new();
    for status in [
        Status::Inbox,
        Status::Active,
        Status::Shipped,
        Status::Rejected,
    ] {
        let dir = specs_dir.join(status.dir());
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some(".gitkeep") {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            // Skip the template itself.
            if path.file_name().and_then(|n| n.to_str()) == Some("_template.md") {
                continue;
            }
            let raw =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            if let Ok(spec) = Spec::parse(&raw, &path) {
                out.push(spec);
            }
        }
    }
    Ok(out)
}

/// Smallest unused 4-digit ID across active / shipped / rejected. We don't
/// recycle IDs even when specs move to rejected — that would confuse
/// references in older specs' `related:` fields.
pub fn next_id(specs_dir: &Path) -> Result<u32> {
    let mut max = 0u32;
    for status in [Status::Active, Status::Shipped, Status::Rejected] {
        let dir = specs_dir.join(status.dir());
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let s = match name.to_str() {
                Some(s) => s,
                None => continue,
            };
            // Look for the leading NNNN- prefix.
            let prefix = s.split('-').next().unwrap_or("");
            if prefix.len() == 4
                && let Ok(n) = prefix.parse::<u32>()
            {
                max = max.max(n);
            }
        }
    }
    Ok(max + 1)
}

/// Create a fresh inbox spec from the template with `title` filled in.
/// Returns the new file's path.
pub fn create_inbox(specs_dir: &Path, title: &str) -> Result<Spec> {
    let title = title.trim();
    if title.is_empty() {
        bail!("spec title cannot be empty");
    }
    let slug = slugify(title);
    if slug.is_empty() {
        bail!("title produced an empty slug; try a more descriptive title");
    }
    let date = today_iso();
    let filename = inbox_filename(&date, &slug);
    let path = specs_dir.join(Status::Inbox.dir()).join(&filename);
    if path.exists() {
        bail!(
            "an inbox spec for {slug:?} already exists at {}",
            path.display()
        );
    }

    let fm = Frontmatter {
        id: None,
        title: title.to_string(),
        status: Some(Status::Inbox),
        owner: "unassigned".into(),
        created: date,
        ..Frontmatter::default()
    };
    let spec = Spec {
        frontmatter: fm,
        body: default_body(title),
        path: path.clone(),
    };
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, spec.serialize()).with_context(|| format!("writing {}", path.display()))?;
    Ok(spec)
}

fn default_body(title: &str) -> String {
    format!(
        "# {title}\n\n\
## Why\n\n\
*Why does this matter? What user-visible or maintainability-visible pain are\n\
we solving? Drop a paragraph or three here.*\n\n\
## What\n\n\
*Each bullet must be verifiable yes/no by a stranger.*\n\n\
- [ ] First acceptance criterion\n\
- [ ] Second acceptance criterion\n\
- [ ] Third acceptance criterion\n\n\
## How\n\n\
*Optional. Implementation sketch + non-obvious decisions.*\n\n\
## Journal\n\n\
- {today}: opened.\n",
        title = title,
        today = today_iso(),
    )
}

/// Move an inbox spec to active/. Assigns the next sequential ID, renames
/// the file to `NNNN-<slug>.md`, and rewrites the frontmatter.
pub fn promote(specs_dir: &Path, spec: &Spec) -> Result<Spec> {
    if !is_in_status(&spec.path, specs_dir, Status::Inbox) {
        bail!(
            "only inbox specs can be promoted, but {} is not in specs/inbox/",
            spec.path.display()
        );
    }
    let id = next_id(specs_dir)?;
    let slug = filename_slug(&spec.path)?;
    let new_filename = numbered_filename(id, &slug);
    let new_path = specs_dir.join(Status::Active.dir()).join(&new_filename);
    if new_path.exists() {
        bail!("target {} already exists", new_path.display());
    }

    let mut new_spec = spec.clone();
    new_spec.frontmatter.id = Some(id);
    new_spec.frontmatter.status = Some(Status::Active);
    new_spec.path = new_path.clone();
    append_journal(
        &mut new_spec,
        &format!("{}: promoted to active.", today_iso()),
    );

    fs::create_dir_all(new_path.parent().unwrap())?;
    fs::write(&new_path, new_spec.serialize())
        .with_context(|| format!("writing {}", new_path.display()))?;
    fs::remove_file(&spec.path).with_context(|| format!("removing old {}", spec.path.display()))?;
    Ok(new_spec)
}

/// Move an active spec to shipped/. Refuses if any `## What` bullet still
/// has `[ ]` — shipping a spec means *all* acceptance criteria pass.
pub fn ship(specs_dir: &Path, spec: &Spec) -> Result<Spec> {
    if !is_in_status(&spec.path, specs_dir, Status::Active) {
        bail!(
            "only active specs can be shipped, but {} is not in specs/active/",
            spec.path.display()
        );
    }
    if !spec.what_all_checked() {
        bail!(
            "cannot ship: not all bullets in `## What` are `[x]`. \
             Open the file, finish the work, then retry."
        );
    }

    let new_path = specs_dir
        .join(Status::Shipped.dir())
        .join(spec.path.file_name().unwrap());
    let mut new_spec = spec.clone();
    new_spec.frontmatter.status = Some(Status::Shipped);
    new_spec.frontmatter.shipped = Some(today_iso());
    new_spec.path = new_path.clone();
    append_journal(&mut new_spec, &format!("{}: shipped.", today_iso()));

    fs::create_dir_all(new_path.parent().unwrap())?;
    fs::write(&new_path, new_spec.serialize())?;
    fs::remove_file(&spec.path)?;
    Ok(new_spec)
}

/// Move a spec (from inbox/ or active/) to rejected/. Records the reason
/// at the bottom of the body so the decision survives.
pub fn reject(specs_dir: &Path, spec: &Spec, reason: &str) -> Result<Spec> {
    let cur_status = current_status(&spec.path, specs_dir).ok_or_else(|| {
        anyhow!(
            "spec at {} is not under a known status directory",
            spec.path.display()
        )
    })?;
    if !matches!(cur_status, Status::Inbox | Status::Active) {
        bail!("only inbox/active specs can be rejected");
    }

    let new_path = specs_dir
        .join(Status::Rejected.dir())
        .join(spec.path.file_name().unwrap());
    let mut new_spec = spec.clone();
    new_spec.frontmatter.status = Some(Status::Rejected);
    new_spec.path = new_path.clone();
    new_spec
        .body
        .push_str(&format!("\n## Reason rejected\n\n{}\n", reason.trim()));
    append_journal(
        &mut new_spec,
        &format!("{}: rejected — {}", today_iso(), reason.trim()),
    );

    fs::create_dir_all(new_path.parent().unwrap())?;
    fs::write(&new_path, new_spec.serialize())?;
    fs::remove_file(&spec.path)?;
    Ok(new_spec)
}

// ---------------------------------------------------------------------------
// Lookup helpers
// ---------------------------------------------------------------------------

/// Find a spec by its numeric ID across active / shipped / rejected.
pub fn find_by_id(specs_dir: &Path, id: u32) -> Result<Option<Spec>> {
    for s in list_all(specs_dir)? {
        if s.frontmatter.id == Some(id) {
            return Ok(Some(s));
        }
    }
    Ok(None)
}

/// Find a spec by a filename fragment (e.g. "streaming-tts" matches
/// `2026-05-13-streaming-tts.md`). Returns the first match by directory
/// search order (inbox → active → shipped → rejected).
pub fn find_by_slug(specs_dir: &Path, query: &str) -> Result<Option<Spec>> {
    let q = query.trim().to_lowercase();
    for s in list_all(specs_dir)? {
        let name = s
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name.contains(&q) {
            return Ok(Some(s));
        }
    }
    Ok(None)
}

/// Try ID first, fall back to slug match. The user can pass either form.
pub fn find(specs_dir: &Path, query: &str) -> Result<Option<Spec>> {
    if let Ok(id) = query.trim().parse::<u32>()
        && let Some(s) = find_by_id(specs_dir, id)?
    {
        return Ok(Some(s));
    }
    find_by_slug(specs_dir, query)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn is_in_status(path: &Path, specs_dir: &Path, status: Status) -> bool {
    current_status(path, specs_dir)
        .map(|s| s == status)
        .unwrap_or(false)
}

fn current_status(path: &Path, specs_dir: &Path) -> Option<Status> {
    let parent = path.parent()?;
    let name = parent.file_name()?.to_str()?;
    // Sanity check that we're actually under specs_dir.
    if parent.parent() != Some(specs_dir) {
        return None;
    }
    Status::parse(name)
}

fn filename_slug(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("filename has no stem"))?;
    // Inbox names are YYYY-MM-DD-slug. Active+ names are NNNN-slug.
    // Strip the leading 4-char chunk + `-` either way.
    let parts: Vec<&str> = stem.splitn(4, '-').collect();
    if parts.len() < 2 {
        return Ok(stem.to_string());
    }
    // Detect inbox shape: 4 numeric chars + `-` + 2 num + `-` + 2 num + `-` + slug.
    if parts.len() >= 4
        && parts[0].len() == 4
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && parts[1].len() == 2
        && parts[1].chars().all(|c| c.is_ascii_digit())
        && parts[2].len() == 2
        && parts[2].chars().all(|c| c.is_ascii_digit())
    {
        return Ok(parts[3].to_string());
    }
    // NNNN-slug shape: drop the first segment.
    if parts[0].len() == 4 && parts[0].chars().all(|c| c.is_ascii_digit()) {
        return Ok(parts[1..].join("-"));
    }
    Ok(stem.to_string())
}

fn append_journal(spec: &mut Spec, entry: &str) {
    // Locate the Journal section. If absent, append one at the end.
    let marker = "## Journal";
    if let Some(pos) = spec.body.find(marker) {
        // Insert the bullet right after the marker line (preserving any
        // existing journal entries below).
        let after_marker = spec.body[pos..]
            .find('\n')
            .map(|n| pos + n + 1)
            .unwrap_or(spec.body.len());
        let mut new_body = String::with_capacity(spec.body.len() + entry.len() + 8);
        new_body.push_str(&spec.body[..after_marker]);
        new_body.push_str(&format!("\n- {entry}\n"));
        new_body.push_str(&spec.body[after_marker..]);
        spec.body = new_body;
    } else {
        spec.body.push_str(&format!("\n## Journal\n\n- {entry}\n"));
    }
}

fn today_iso() -> String {
    // We avoid pulling in `chrono` for one date. The kernel's CLOCK_REALTIME
    // converted to UTC YYYY-MM-DD is enough.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = ymd_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Tiny civil-date conversion (no leap seconds; good enough for filenames).
/// Based on Howard Hinnant's days_from_civil inverse — same math the C++
/// standard library uses.
fn ymd_from_unix(secs: u64) -> (u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let specs = tmp.path().join("specs");
        for sub in &["inbox", "active", "shipped", "rejected"] {
            fs::create_dir_all(specs.join(sub)).unwrap();
        }
        (tmp, specs)
    }

    #[test]
    fn create_inbox_writes_file() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "Hola Mutombo").unwrap();
        assert!(s.path.exists());
        let raw = fs::read_to_string(&s.path).unwrap();
        assert!(raw.contains("title: Hola Mutombo"));
        assert!(raw.contains("status: inbox"));
        assert!(raw.contains("## Why"));
    }

    #[test]
    fn next_id_starts_at_one_in_empty_repo() {
        let (_t, specs) = fixture();
        assert_eq!(next_id(&specs).unwrap(), 1);
    }

    #[test]
    fn promote_assigns_next_id_and_moves_file() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "First Spec").unwrap();
        let promoted = promote(&specs, &s).unwrap();
        assert_eq!(promoted.frontmatter.id, Some(1));
        assert!(promoted.path.exists());
        assert!(!s.path.exists());
        assert!(promoted.path.starts_with(specs.join("active")));
    }

    #[test]
    fn ship_refuses_when_what_has_open_bullets() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "Unfinished").unwrap();
        let promoted = promote(&specs, &s).unwrap();
        // Default template has `- [ ]` bullets.
        let err = ship(&specs, &promoted).unwrap_err();
        assert!(err.to_string().contains("not all bullets"));
    }

    #[test]
    fn ship_succeeds_when_all_bullets_checked() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "Done thing").unwrap();
        let mut promoted = promote(&specs, &s).unwrap();
        promoted.body = promoted.body.replace("- [ ]", "- [x]");
        fs::write(&promoted.path, promoted.serialize()).unwrap();
        let reloaded =
            Spec::parse(&fs::read_to_string(&promoted.path).unwrap(), &promoted.path).unwrap();
        let shipped = ship(&specs, &reloaded).unwrap();
        assert_eq!(shipped.frontmatter.status, Some(Status::Shipped));
        assert!(shipped.frontmatter.shipped.is_some());
    }

    #[test]
    fn reject_works_from_inbox() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "Bad idea").unwrap();
        let rejected = reject(&specs, &s, "doesn't fit the project").unwrap();
        assert_eq!(rejected.frontmatter.status, Some(Status::Rejected));
        assert!(rejected.body.contains("Reason rejected"));
        assert!(rejected.body.contains("doesn't fit the project"));
    }

    #[test]
    fn find_by_id_and_slug() {
        let (_t, specs) = fixture();
        let s = create_inbox(&specs, "Streaming TTS").unwrap();
        let p = promote(&specs, &s).unwrap();
        let id = p.frontmatter.id.unwrap();
        assert!(find_by_id(&specs, id).unwrap().is_some());
        assert!(find_by_slug(&specs, "streaming-tts").unwrap().is_some());
        assert!(find(&specs, &id.to_string()).unwrap().is_some());
        assert!(find(&specs, "streaming").unwrap().is_some());
    }

    #[test]
    fn ymd_known_dates() {
        // 2026-05-13 UTC midnight in unix seconds. Computed independently
        // (any online epoch converter agrees) so we lock the algorithm
        // against accidental drift.
        let ts = 1_778_630_400;
        assert_eq!(ymd_from_unix(ts), (2026, 5, 13));
        // Sanity check a couple of well-known dates.
        assert_eq!(ymd_from_unix(0), (1970, 1, 1));
        assert_eq!(ymd_from_unix(946_684_800), (2000, 1, 1));
    }
}
