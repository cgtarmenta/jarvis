//! `WorkerManifest` — the data backing each TOML file in
//! `~/.config/jarvis/workers/`.
//!
//! Spec 0008 calls for one TOML per worker, autodiscovered at daemon
//! startup, with placeholder substitution (`{prompt}` / `{session_id}` /
//! `{cwd}`) into the command vector at spawn time. This module owns the
//! parse + validate + substitute primitives. Autodiscovery and the
//! runtime `WorkerRegistry` land in C-2; this commit is the schema layer.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// Where to read the captured session id from when running a stateful
/// worker. v1 supports stdout / stderr regex; other sources (env var,
/// file path) are out of scope for spec 0008.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionIdSource {
    Stdout,
    Stderr,
}

/// Stateful workers can emit a session id we should remember and pass
/// back on the next invocation via the `{session_id}` placeholder. The
/// regex must contain exactly one capture group; the captured value is
/// stored verbatim in the session's `active_workers` map.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdCapture {
    pub source: SessionIdSource,
    pub regex: String,
}

/// Declarative worker definition loaded from a single
/// `~/.config/jarvis/workers/<id>.toml` file.
///
/// Field semantics:
/// - `command` is required, always used after a session id is known (or
///   if the worker is stateless). Placeholders: `{prompt}`,
///   `{session_id}`, `{cwd}`.
/// - `initial_command` is consulted only when `stateful = true` *and*
///   no session id is yet known for this worker on the current thread.
///   Falls back to `command` if not provided.
/// - `stateful = true` plus a missing `session_id_capture` is allowed
///   but flagged as a warning at load time — it works for workers that
///   share a single global session (`claude --resume <uuid>` knows the
///   uuid out-of-band), just not for capture-on-first-invocation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerManifest {
    pub id: String,

    #[serde(default)]
    pub description: Option<String>,

    pub command: Vec<String>,

    #[serde(default)]
    pub initial_command: Option<Vec<String>>,

    #[serde(default)]
    pub stateful: bool,

    #[serde(default)]
    pub session_id_capture: Option<SessionIdCapture>,

    #[serde(default)]
    pub async_eligible: bool,

    #[serde(default)]
    pub tty: bool,

    #[serde(default)]
    pub dispatch_hint: Option<String>,
}

/// The placeholders the dispatcher knows how to substitute. Adding one
/// here is a breaking schema change for manifest authors — keep the set
/// small and motivated. Out-of-set placeholders fail manifest
/// validation, on purpose, so typos surface at load time instead of at
/// spawn time with garbled command lines.
pub const KNOWN_PLACEHOLDERS: &[&str] = &["prompt", "session_id", "cwd"];

impl WorkerManifest {
    /// Parse + validate a manifest from a TOML string. Errors describe
    /// the offending field with the worker id so a directory full of
    /// manifests stays diagnosable.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let m: WorkerManifest = toml::from_str(s).context("parsing worker manifest TOML")?;
        m.validate()?;
        Ok(m)
    }

    /// Validation called automatically by `from_toml_str`. Public so the
    /// future registry can re-check after edits.
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(anyhow!("worker manifest: `id` must not be empty"));
        }
        if self.command.is_empty() {
            return Err(anyhow!("worker {:?}: `command` must not be empty", self.id));
        }
        validate_placeholders(&self.command, &self.id, "command")?;
        if let Some(init) = &self.initial_command
            && !init.is_empty()
        {
            validate_placeholders(init, &self.id, "initial_command")?;
        }
        Ok(())
    }

    /// Build the concrete command vector to spawn this worker.
    ///
    /// When `for_initial = true` *and* the worker is stateful *and* an
    /// `initial_command` is configured, that variant is used; otherwise
    /// `command` is used. Every placeholder in the chosen template is
    /// substituted with the value from `values` keyed by the inner name
    /// (e.g. `{prompt}` → `values["prompt"]`).
    ///
    /// Placeholders absent from `values` are passed through unchanged.
    /// In practice the dispatcher should always supply every known
    /// placeholder (use empty string for "not applicable"); pass-through
    /// is the safe fallback rather than a panic.
    pub fn build_command(&self, values: &HashMap<&str, &str>, for_initial: bool) -> Vec<String> {
        let template: &[String] = if for_initial
            && self.stateful
            && self
                .initial_command
                .as_deref()
                .is_some_and(|init| !init.is_empty())
        {
            self.initial_command.as_deref().unwrap()
        } else {
            &self.command
        };
        template.iter().map(|arg| substitute(arg, values)).collect()
    }
}

fn validate_placeholders(template: &[String], worker_id: &str, field: &'static str) -> Result<()> {
    for arg in template {
        for placeholder in scan_placeholders(arg) {
            if !KNOWN_PLACEHOLDERS.contains(&placeholder.as_str()) {
                return Err(anyhow!(
                    "worker {:?}: unknown placeholder {{{}}} in `{}`; \
                     valid placeholders are: {}",
                    worker_id,
                    placeholder,
                    field,
                    KNOWN_PLACEHOLDERS.join(", "),
                ));
            }
        }
    }
    Ok(())
}

/// Find every `{name}` placeholder in `s`, returning the names (no braces).
/// Unmatched `{` (no closing `}`) is treated as not-a-placeholder and the
/// search stops there — same rule the substituter uses, so they agree on
/// what counts.
fn scan_placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(open_rel) = s[search_from..].find('{') {
        let open = search_from + open_rel;
        let Some(close_rel) = s[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_rel;
        let name = &s[open + 1..close];
        if !name.is_empty() {
            out.push(name.to_string());
        }
        search_from = close + 1;
    }
    out
}

/// Replace each `{name}` in `s` with `values[name]`. Names not in
/// `values` are passed through verbatim (validation upstream guarantees
/// known placeholders are recognised; missing values mean the caller
/// intentionally omitted that key).
pub fn substitute(s: &str, values: &HashMap<&str, &str>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut search_from = 0;
    while let Some(open_rel) = s[search_from..].find('{') {
        let open = search_from + open_rel;
        let Some(close_rel) = s[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_rel;
        let name = &s[open + 1..close];
        out.push_str(&s[last..open]);
        if let Some(v) = values.get(name) {
            out.push_str(v);
        } else {
            out.push_str(&s[open..close + 1]);
        }
        last = close + 1;
        search_from = last;
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals<'a>(pairs: &'a [(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
        pairs.iter().copied().collect()
    }

    /// Spec 0008: a manifest with all documented fields parses cleanly
    /// and round-trips its values. Locks the schema down — any rename
    /// or removal becomes a test failure rather than a silent surprise.
    #[test]
    fn parse_full_manifest() {
        let toml = r#"
            id = "claude"
            description = "Claude Code session"
            command = ["claude", "--print", "--resume", "{session_id}"]
            initial_command = ["claude", "--print"]
            stateful = true
            session_id_capture = { source = "stdout", regex = "Session: ([a-f0-9-]+)" }
            async_eligible = true
            tty = false
            dispatch_hint = "Best for coding tasks."
        "#;
        let m = WorkerManifest::from_toml_str(toml).expect("parse full");
        assert_eq!(m.id, "claude");
        assert_eq!(m.description.as_deref(), Some("Claude Code session"));
        assert!(m.stateful);
        assert!(m.async_eligible);
        assert!(!m.tty);
        let cap = m.session_id_capture.expect("capture present");
        assert_eq!(cap.source, SessionIdSource::Stdout);
        assert_eq!(cap.regex, "Session: ([a-f0-9-]+)");
    }

    /// Spec 0008: only `id` and `command` are required. Defaults fill in
    /// the rest. Verifies the `#[serde(default)]` annotations.
    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
            id = "time"
            command = ["jarvis-handler-time", "{prompt}"]
        "#;
        let m = WorkerManifest::from_toml_str(toml).expect("parse minimal");
        assert_eq!(m.id, "time");
        assert!(!m.stateful);
        assert!(!m.async_eligible);
        assert!(!m.tty);
        assert!(m.description.is_none());
        assert!(m.dispatch_hint.is_none());
    }

    /// Spec 0008: unknown placeholders fail at load time, not spawn time.
    /// This is the contract that makes typos in manifests surface
    /// loud when the daemon starts up.
    #[test]
    fn reject_unknown_placeholder() {
        let toml = r#"
            id = "broken"
            command = ["echo", "{user_input}"]
        "#;
        let err = WorkerManifest::from_toml_str(toml).expect_err("should reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown placeholder"), "got: {msg}");
        assert!(msg.contains("user_input"), "got: {msg}");
        assert!(msg.contains("broken"), "got: {msg}");
    }

    /// Spec 0008: empty `command` array fails validation.
    #[test]
    fn reject_empty_command() {
        let toml = r#"
            id = "broken"
            command = []
        "#;
        let err = WorkerManifest::from_toml_str(toml).expect_err("should reject");
        assert!(format!("{err:#}").contains("must not be empty"));
    }

    /// Spec 0008: `deny_unknown_fields` catches typos at the top level
    /// of the manifest. A misspelled field name should surface at load,
    /// not silently get ignored.
    #[test]
    fn reject_unknown_top_level_field() {
        let toml = r#"
            id = "claude"
            command = ["claude"]
            statefull = true   # typo: extra 'l'
        "#;
        let err = WorkerManifest::from_toml_str(toml).expect_err("should reject typo");
        assert!(format!("{err:#}").contains("statefull"));
    }

    /// Substitution: every supported placeholder is replaced, ordering
    /// preserved, non-placeholder text untouched.
    #[test]
    fn substitute_replaces_known_placeholders() {
        let v = vals(&[("prompt", "hola"), ("session_id", "abc"), ("cwd", "/home")]);
        assert_eq!(substitute("--prompt={prompt}", &v), "--prompt=hola");
        assert_eq!(substitute("{cwd}/sub", &v), "/home/sub");
        assert_eq!(substitute("[{session_id}]", &v), "[abc]");
    }

    /// Missing values pass through unchanged (validator enforces that
    /// only known names appear; missing-at-spawn means the dispatcher
    /// intentionally left a field empty).
    #[test]
    fn substitute_passes_through_missing_value() {
        let v = vals(&[("prompt", "hola")]);
        assert_eq!(substitute("--id={session_id}", &v), "--id={session_id}");
    }

    /// Build_command picks initial_command on first invocation of a
    /// stateful worker, command otherwise. Covers the four combinations
    /// of (stateful, for_initial) plus the no-initial fallback case.
    #[test]
    fn build_command_picks_template_correctly() {
        let m = WorkerManifest {
            id: "claude".to_string(),
            description: None,
            command: vec!["claude".to_string(), "--resume".to_string(), "{session_id}".to_string()],
            initial_command: Some(vec!["claude".to_string(), "--print".to_string()]),
            stateful: true,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let v = vals(&[("session_id", "abc-123")]);

        // Stateful + for_initial=true and initial_command present → initial.
        assert_eq!(
            m.build_command(&v, true),
            vec!["claude".to_string(), "--print".to_string()]
        );

        // Stateful + for_initial=false → command (with substitution).
        assert_eq!(
            m.build_command(&v, false),
            vec!["claude".to_string(), "--resume".to_string(), "abc-123".to_string()]
        );

        // Stateless: for_initial flag is ignored, command always wins.
        let mut stateless = m.clone();
        stateless.stateful = false;
        assert_eq!(
            stateless.build_command(&v, true),
            vec!["claude".to_string(), "--resume".to_string(), "abc-123".to_string()]
        );

        // Stateful but no initial_command: fall back to command.
        let mut no_init = m.clone();
        no_init.initial_command = None;
        assert_eq!(
            no_init.build_command(&v, true),
            vec!["claude".to_string(), "--resume".to_string(), "abc-123".to_string()]
        );
    }

    /// UTF-8 safety: a Spanish-language manifest with non-ASCII
    /// description and non-ASCII text outside placeholders survives
    /// substitution intact. Byte-level placeholder scanning has to
    /// avoid corrupting multi-byte characters.
    #[test]
    fn substitute_preserves_non_ascii() {
        let v = vals(&[("prompt", "saludo")]);
        assert_eq!(substitute("¿{prompt}? — sí", &v), "¿saludo? — sí");
    }

    /// scan_placeholders finds the names without braces. Unmatched
    /// `{` halts scanning to keep validator and substituter aligned.
    #[test]
    fn scan_placeholders_basic() {
        assert_eq!(
            scan_placeholders("{a}-{b}-{c}"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(scan_placeholders("no placeholders"), Vec::<String>::new());
        assert_eq!(scan_placeholders("oops {unclosed"), Vec::<String>::new());
        assert_eq!(scan_placeholders("{}"), Vec::<String>::new()); // empty name skipped
    }
}
