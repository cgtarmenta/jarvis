//! `WorkerRegistry` — autodiscovers `~/.config/jarvis/workers/*.toml`,
//! parses + validates each manifest, and exposes the result as a queryable
//! collection of active and disabled workers.
//!
//! Spec 0008's load-time contract: malformed manifests, missing binaries,
//! duplicate ids, and bad regex never crash the daemon — they get
//! recorded as `DisabledWorker` entries with a human-readable reason so
//! `jarvis worker list` (C-5) and `jarvis doctor` can show the user what
//! went wrong without forcing them to read journald.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

use super::manifest::WorkerManifest;
use crate::config;

/// A manifest that failed to load for any reason. Kept around so users can
/// see at a glance which files are broken and why.
#[derive(Debug, Clone)]
pub struct DisabledWorker {
    /// The TOML file we tried to load. Always present, even when the
    /// failure means we never extracted an id.
    pub source: PathBuf,
    /// The id from the manifest if we managed to parse that far; `None`
    /// if parse failed before reaching the id.
    pub id: Option<String>,
    /// Human-readable explanation: "binary 'foo' not on PATH",
    /// "duplicate id 'claude' (already loaded from …)", etc.
    pub reason: String,
}

/// The result of scanning a worker manifest directory.
#[derive(Debug, Default)]
pub struct WorkerRegistry {
    /// Active workers, keyed by manifest id. Insertion order is preserved
    /// — the autodiscovery sort order (alphabetical by filename) matters
    /// because the dispatcher uses it as a tie-break for hints.
    active: Vec<WorkerManifest>,
    /// Index from id → position in `active`. Cheap lookups without
    /// disturbing iteration order.
    by_id: HashMap<String, usize>,
    /// Manifests that couldn't be loaded. Reported by `jarvis worker
    /// list`; never silently ignored.
    disabled: Vec<DisabledWorker>,
}

impl WorkerRegistry {
    /// Default location: the same directory as the user's main config
    /// file (`~/.config/jarvis/`) with a `workers/` subdirectory. The
    /// directory does *not* have to exist yet — `load_from_dir` treats
    /// missing as "no workers configured".
    pub fn default_dir() -> Result<PathBuf> {
        let cfg = config::config_path()?;
        let parent = cfg
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", cfg.display()))?;
        Ok(parent.join("workers"))
    }

    /// Scan `dir` for `*.toml` files. Each file is loaded independently;
    /// a failure on one does not affect the others. Missing dir → empty
    /// registry. Always returns a registry (never an error) so the
    /// daemon never refuses to start because of a worker manifest.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut reg = Self::default();
        let read = match fs::read_dir(dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No workers/ directory at all → empty registry.
                return reg;
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "reading workers dir");
                return reg;
            }
        };

        // Sort entries by filename so registry ordering is reproducible
        // across runs; helpful for tests and for the `worker list` output.
        let mut paths: Vec<PathBuf> = read
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
            .collect();
        paths.sort();

        for path in paths {
            reg.load_one(&path);
        }
        reg
    }

    fn load_one(&mut self, path: &Path) {
        let raw = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                self.disabled.push(DisabledWorker {
                    source: path.to_path_buf(),
                    id: None,
                    reason: format!("read error: {e}"),
                });
                return;
            }
        };

        let manifest = match WorkerManifest::from_toml_str(&raw) {
            Ok(m) => m,
            Err(e) => {
                // Try to recover the id from a partial parse so the
                // disabled entry is more useful in `worker list`.
                let id = peek_id(&raw);
                self.disabled.push(DisabledWorker {
                    source: path.to_path_buf(),
                    id,
                    reason: format!("{e:#}"),
                });
                return;
            }
        };

        // Cross-manifest checks: id uniqueness. We don't track source
        // paths for active manifests in v1, so the conflict message
        // just names the offending id — `worker list` can fill in
        // the surrounding picture.
        if self.by_id.contains_key(&manifest.id) {
            self.disabled.push(DisabledWorker {
                source: path.to_path_buf(),
                id: Some(manifest.id.clone()),
                reason: format!(
                    "duplicate id {:?}: an earlier manifest with this id is already loaded",
                    manifest.id
                ),
            });
            return;
        }

        // Compile session_id_capture regex once at load time so the
        // dispatcher never has to. Bad regex disables the worker.
        if let Some(cap) = &manifest.session_id_capture
            && let Err(e) = regex::Regex::new(&cap.regex)
        {
            self.disabled.push(DisabledWorker {
                source: path.to_path_buf(),
                id: Some(manifest.id.clone()),
                reason: format!("session_id_capture.regex did not compile: {e}"),
            });
            return;
        }

        // Binary presence on PATH. If the user typoed the command or
        // hasn't installed the wrapped tool, we want to show that
        // *now*, not when they try to use the worker.
        let bin = &manifest.command[0];
        if !is_executable_present(bin) {
            self.disabled.push(DisabledWorker {
                source: path.to_path_buf(),
                id: Some(manifest.id.clone()),
                reason: format!("binary {bin:?} not found on PATH"),
            });
            return;
        }

        // All checks passed: register active.
        self.by_id.insert(manifest.id.clone(), self.active.len());
        self.active.push(manifest);
    }

    /// Lookup an active worker by id. Disabled workers are *not* returned —
    /// the dispatcher should treat them as nonexistent.
    pub fn get(&self, id: &str) -> Option<&WorkerManifest> {
        self.by_id.get(id).map(|&i| &self.active[i])
    }

    pub fn active_workers(&self) -> &[WorkerManifest] {
        &self.active
    }

    pub fn disabled_workers(&self) -> &[DisabledWorker] {
        &self.disabled
    }

    /// Total count of manifest files we touched (active + disabled).
    /// Useful for the "Loaded N workers (M disabled)" log line.
    pub fn total_seen(&self) -> usize {
        self.active.len() + self.disabled.len()
    }
}

/// Best-effort extraction of an id from a TOML string that failed full
/// validation. Useful for error messages on partially-broken manifests.
fn peek_id(raw: &str) -> Option<String> {
    let table: toml::Value = toml::from_str(raw).ok()?;
    table
        .as_table()?
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Treat absolute paths as executables iff the file exists and is a
/// regular file. Relative names go through PATH lookup. This is the
/// same shape as the existing claude-binary check in `claude.rs`.
fn is_executable_present(name: &str) -> bool {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.is_file();
    }
    which::which(name).is_ok()
}

/// Convenience: load the default workers/ directory. Equivalent to
/// `load_from_dir(default_dir()?)`, with the missing-dir case mapped to
/// an empty registry.
pub fn load_default() -> Result<WorkerRegistry> {
    let dir = WorkerRegistry::default_dir()
        .context("resolving default workers directory")?;
    Ok(WorkerRegistry::load_from_dir(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).expect("write fixture");
    }

    /// Spec 0008: missing workers directory → empty registry, daemon
    /// still starts. Mirrors the "zero valid manifests" line in the
    /// `## What` bullets.
    #[test]
    fn missing_dir_yields_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let reg = WorkerRegistry::load_from_dir(&nonexistent);
        assert!(reg.active_workers().is_empty());
        assert!(reg.disabled_workers().is_empty());
    }

    /// A valid manifest pointing at a binary that exists on the test
    /// host (`sh` is universally present) loads as active.
    #[test]
    fn valid_manifest_loads_active() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "shell.toml",
            r#"
                id = "shell"
                command = ["sh", "-c", "{prompt}"]
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.active_workers().len(), 1);
        assert!(reg.disabled_workers().is_empty());
        assert!(reg.get("shell").is_some());
    }

    /// Malformed TOML → disabled, not crashing. The reason mentions the
    /// nature of the failure so `worker list` is useful.
    #[test]
    fn malformed_toml_disables_worker() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "broken.toml", "this = is = not = toml");
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert!(reg.active_workers().is_empty());
        assert_eq!(reg.disabled_workers().len(), 1);
        assert!(
            reg.disabled_workers()[0]
                .reason
                .to_lowercase()
                .contains("parsing")
                || reg.disabled_workers()[0]
                    .reason
                    .to_lowercase()
                    .contains("toml")
        );
    }

    /// Unknown placeholder in command: manifest itself parses, validation
    /// fails. Verifies the disabled-entry id is still recovered.
    #[test]
    fn unknown_placeholder_disables_worker() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "bad.toml",
            r#"
                id = "bad"
                command = ["sh", "{user_input}"]
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert!(reg.active_workers().is_empty());
        let disabled = &reg.disabled_workers()[0];
        assert_eq!(disabled.id.as_deref(), Some("bad"));
        assert!(disabled.reason.contains("unknown placeholder"));
    }

    /// A manifest referencing a binary not on PATH is rejected with a
    /// reason that names the missing binary. Catches the typo case
    /// (`comand = ["cluade"]`) before the user tries to use it.
    #[test]
    fn missing_binary_disables_worker() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "nope.toml",
            r#"
                id = "nope"
                command = ["this-binary-definitely-does-not-exist-12345", "{prompt}"]
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert!(reg.active_workers().is_empty());
        let disabled = &reg.disabled_workers()[0];
        assert_eq!(disabled.id.as_deref(), Some("nope"));
        assert!(disabled.reason.contains("not found on PATH"));
    }

    /// Two manifests with the same id: the first to load (alphabetical by
    /// filename) wins; the second is disabled with a duplicate-id reason.
    #[test]
    fn duplicate_id_disables_second_occurrence() {
        let tmp = TempDir::new().unwrap();
        // Alphabetical ordering: a-claude.toml loads first, b-claude.toml second.
        write(
            tmp.path(),
            "a-claude.toml",
            r#"
                id = "claude"
                command = ["sh", "-c", "{prompt}"]
            "#,
        );
        write(
            tmp.path(),
            "b-claude.toml",
            r#"
                id = "claude"
                command = ["sh", "-c", "{prompt}"]
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.active_workers().len(), 1);
        assert_eq!(reg.disabled_workers().len(), 1);
        let disabled = &reg.disabled_workers()[0];
        assert_eq!(disabled.id.as_deref(), Some("claude"));
        assert!(disabled.reason.contains("duplicate id"));
        assert!(disabled.source.ends_with("b-claude.toml"));
    }

    /// Bad regex in session_id_capture: parses but doesn't compile →
    /// disabled with a clear reason.
    #[test]
    fn bad_capture_regex_disables_worker() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "regex.toml",
            r#"
                id = "regex"
                command = ["sh", "-c", "{prompt}"]
                stateful = true
                session_id_capture = { source = "stdout", regex = "[unclosed" }
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert!(reg.active_workers().is_empty());
        let disabled = &reg.disabled_workers()[0];
        assert_eq!(disabled.id.as_deref(), Some("regex"));
        assert!(disabled.reason.to_lowercase().contains("regex"));
    }

    /// Mix of good and bad: the good ones still load. Validates that
    /// one bad apple doesn't spoil the registry.
    #[test]
    fn good_and_bad_coexist() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "good.toml",
            r#"
                id = "good"
                command = ["sh", "-c", "{prompt}"]
            "#,
        );
        write(tmp.path(), "broken.toml", "garbage = = =");
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.active_workers().len(), 1);
        assert_eq!(reg.disabled_workers().len(), 1);
        assert_eq!(reg.total_seen(), 2);
    }

    /// Non-toml files in the workers dir are ignored. Common case: a
    /// README.md or backup file the user dropped in.
    #[test]
    fn non_toml_files_are_ignored() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "README.md", "not a manifest");
        write(tmp.path(), "good.toml.bak", "id = \"backup\"\ncommand = [\"sh\"]\n");
        write(
            tmp.path(),
            "good.toml",
            r#"
                id = "good"
                command = ["sh", "-c", "{prompt}"]
            "#,
        );
        let reg = WorkerRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.active_workers().len(), 1);
        assert!(reg.disabled_workers().is_empty());
    }
}
