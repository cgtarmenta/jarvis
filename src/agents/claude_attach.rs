//! Plumbing for resuming a Claude Code session from the `claude` agent.
//!
//! Claude Code stores each session as a JSONL transcript under
//! `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`. The CLI exposes
//! `claude --print --resume <uuid>` to continue any session. This module
//! owns the bridge between Jarvis and that layout:
//!
//! * **Path encoding** — `/home/dat30/github/foo` → `-home-dat30-github-foo`.
//!   Reverse-engineered from the on-disk layout; isolated behind one
//!   function so a future Anthropic format change has one place to patch.
//! * **Session listing** — every JSONL under `~/.claude/projects/`,
//!   sorted newest-first by mtime. Used by `jarvis claude sessions` and
//!   by auto-resume.
//! * **Attachment state** — a small TOML file at
//!   `$XDG_CACHE_HOME/jarvis/claude-attach.toml` that overrides the
//!   `[agent]` config for the current pinned session.
//! * **Resolution** — picks the right `Attachment` to apply at agent
//!   construction time given config + state file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Where the user's Claude Code installs its session files. Configurable
/// via `JARVIS_CLAUDE_HOME` env var purely so the unit tests can isolate
/// themselves from the real on-disk state without resorting to monkey-
/// patched globals.
pub fn claude_home() -> PathBuf {
    if let Ok(p) = std::env::var("JARVIS_CLAUDE_HOME") {
        return PathBuf::from(p);
    }
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".claude")
}

/// Directory holding session transcripts for one Claude Code project.
pub fn project_dir_for(cwd: &Path) -> PathBuf {
    claude_home()
        .join("projects")
        .join(encode_project_path(cwd))
}

/// Translate a working directory into the encoded form Claude Code uses
/// for its on-disk project namespace. Pure string transform — we don't
/// hit the filesystem here.
///
/// Claude joins the path components with `-` and prefixes with `-` so
/// `/home/foo/bar` becomes `-home-foo-bar`. The exact algorithm isn't
/// documented but matches what `ls ~/.claude/projects/` shows.
pub fn encode_project_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    // `Path::components()` would strip empty segments; we want every
    // `/` to become a `-` including the leading one.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '/' {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

/// One Claude Code session, the way `jarvis claude sessions` lists them.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub uuid: String,
    pub path: PathBuf,
    /// `unix` seconds — `0` if mtime couldn't be read.
    pub mtime: u64,
    pub size_bytes: u64,
    /// First user turn extracted from the JSONL, truncated to ~120 chars.
    /// Useful as a "what was this session about?" preview. Empty string
    /// if we couldn't find one.
    pub first_user_message: String,
}

/// List sessions across every project on disk, newest first. When
/// `filter_cwd` is `Some`, only the project for that cwd is included.
pub fn list_sessions(filter_cwd: Option<&Path>) -> Result<Vec<SessionInfo>> {
    let root = claude_home().join("projects");
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let target_subdir = filter_cwd.map(encode_project_path);
    let mut out = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))? {
        let entry = entry?;
        let project = entry.path();
        if !project.is_dir() {
            continue;
        }
        if let Some(want) = &target_subdir
            && project.file_name().and_then(|n| n.to_str()) != Some(want.as_str())
        {
            continue;
        }
        for jentry in fs::read_dir(&project)? {
            let jentry = jentry?;
            let p = jentry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(s) = read_session_info(&p) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    Ok(out)
}

/// Most-recently-modified session under `cwd`'s project namespace.
/// Returns `None` if there's no such directory or no JSONL files.
pub fn latest_session_for(cwd: &Path) -> Option<SessionInfo> {
    list_sessions(Some(cwd)).ok()?.into_iter().next()
}

fn read_session_info(path: &Path) -> Option<SessionInfo> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let uuid = path.file_stem()?.to_str()?.to_string();
    Some(SessionInfo {
        uuid,
        path: path.to_path_buf(),
        mtime,
        size_bytes: meta.len(),
        first_user_message: first_user_message(path).unwrap_or_default(),
    })
}

/// Best-effort: scan the JSONL until we find an entry whose
/// `"type": "user"` field has a `"text"` payload, return it truncated.
/// Failures (missing field, weird format) return None rather than
/// blowing up the list view.
fn first_user_message(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = fs::File::open(path).ok()?;
    let reader = BufReader::new(f);
    for line in reader.lines().map_while(|l| l.ok()).take(200) {
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).ok()?;
        let text = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| {
                if let Some(s) = c.as_str() {
                    Some(s.to_string())
                } else if let Some(arr) = c.as_array() {
                    arr.iter()
                        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                        .next()
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })?;
        let truncated: String = text.chars().take(120).collect();
        return Some(truncated);
    }
    None
}

// ---------------------------------------------------------------------------
// Attachment state file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttachState {
    /// Pinned session uuid (highest priority).
    #[serde(default)]
    pub session_id: Option<String>,
    /// If `true`, ignore `session_id` and pick the newest JSONL in `cwd`
    /// every time the agent constructs.
    #[serde(default)]
    pub auto_resume: bool,
    /// Optional override for the working directory; falls back to the
    /// agent's `[agent].cwd` if `None`.
    #[serde(default)]
    pub cwd: Option<String>,
}

pub fn state_path() -> Result<PathBuf> {
    let dir = crate::config::cache_dir()?;
    fs::create_dir_all(&dir)?;
    Ok(dir.join("claude-attach.toml"))
}

pub fn load_state() -> Result<Option<AttachState>> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let st: AttachState =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(st))
}

pub fn save_state(state: &AttachState) -> Result<()> {
    let path = state_path()?;
    let tmp = path.with_extension("toml.tmp");
    let body = toml::to_string_pretty(state).context("serialising attach state")?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn clear_state() -> Result<()> {
    let path = state_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// What the Claude agent should do for the current turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// Stateless `claude --print`.
    None,
    /// Resume an exact session.
    Pinned(String),
    /// Resume whichever session is newest in `cwd`. Resolved at each
    /// `respond()` call so the agent stays current as Claude writes
    /// new turns.
    Latest { cwd: PathBuf },
}

/// Decide which `Attachment` applies given the state file and the
/// config-level `cwd` / `auto_resume` knobs. State file wins; config is
/// the persistent default.
pub fn resolve(
    state: Option<&AttachState>,
    config_cwd: Option<&str>,
    config_auto_resume: bool,
) -> Attachment {
    if let Some(s) = state {
        if let Some(uuid) = &s.session_id {
            return Attachment::Pinned(uuid.clone());
        }
        if s.auto_resume {
            let cwd = s
                .cwd
                .as_deref()
                .or(config_cwd)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            return Attachment::Latest { cwd };
        }
    }
    if config_auto_resume {
        let cwd = config_cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return Attachment::Latest { cwd };
    }
    Attachment::None
}

impl Attachment {
    /// Convert to a UUID by resolving `Latest` against the filesystem.
    /// `Pinned` is returned as-is. `None` returns `None`.
    pub fn to_uuid(&self) -> Option<String> {
        match self {
            Attachment::None => None,
            Attachment::Pinned(uuid) => Some(uuid.clone()),
            Attachment::Latest { cwd } => latest_session_for(cwd).map(|s| s.uuid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::Duration;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        // SAFETY: tests run with `serial_test::serial` upstream — this is
        // the same env-var trick we use for XDG redirection.
        unsafe {
            std::env::set_var("JARVIS_CLAUDE_HOME", tmp.path().join(".claude"));
        }
        let project_subdir = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&project_subdir).unwrap();
        (tmp, project_subdir)
    }

    #[test]
    fn encode_matches_known_layout() {
        assert_eq!(
            encode_project_path(Path::new("/home/dat30/github/jarvis")),
            "-home-dat30-github-jarvis"
        );
        assert_eq!(encode_project_path(Path::new("/")), "-");
    }

    #[test]
    #[serial]
    fn list_sessions_returns_newest_first() {
        let (_t, projects) = fixture();
        let proj = projects.join("-home-foo");
        fs::create_dir_all(&proj).unwrap();
        // Two sessions; manipulate mtime so order is deterministic.
        let older = proj.join("old.jsonl");
        let newer = proj.join("new.jsonl");
        fs::write(&older, "{\"type\":\"user\"}\n").unwrap();
        // Sleep enough for mtime granularity to register on most FSes.
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(&newer, "{\"type\":\"user\"}\n").unwrap();
        let all = list_sessions(None).unwrap();
        assert!(all.len() >= 2);
        assert_eq!(all[0].uuid, "new");
        assert_eq!(all[1].uuid, "old");
    }

    #[test]
    #[serial]
    fn latest_for_cwd_filters_correctly() {
        let (_t, projects) = fixture();
        fs::create_dir_all(projects.join("-home-foo")).unwrap();
        fs::create_dir_all(projects.join("-home-bar")).unwrap();
        fs::write(projects.join("-home-foo").join("a.jsonl"), "{}\n").unwrap();
        fs::write(projects.join("-home-bar").join("b.jsonl"), "{}\n").unwrap();
        let latest = latest_session_for(Path::new("/home/foo")).unwrap();
        assert_eq!(latest.uuid, "a");
    }

    #[test]
    fn resolve_priority() {
        // Pinned beats auto_resume in state file
        let st = AttachState {
            session_id: Some("xxx".into()),
            auto_resume: true,
            cwd: Some("/x".into()),
        };
        assert_eq!(
            resolve(Some(&st), None, false),
            Attachment::Pinned("xxx".to_string())
        );

        // auto_resume in state file uses state cwd
        let st = AttachState {
            session_id: None,
            auto_resume: true,
            cwd: Some("/from-state".into()),
        };
        match resolve(Some(&st), Some("/from-config"), false) {
            Attachment::Latest { cwd } => assert_eq!(cwd, PathBuf::from("/from-state")),
            _ => panic!("expected Latest"),
        }

        // No state, config-level auto_resume = true
        match resolve(None, Some("/from-config"), true) {
            Attachment::Latest { cwd } => assert_eq!(cwd, PathBuf::from("/from-config")),
            _ => panic!("expected Latest"),
        }

        // Nothing set
        assert_eq!(resolve(None, None, false), Attachment::None);
    }

    #[test]
    #[serial]
    fn state_roundtrips() {
        let (_t, _projects) = fixture();
        let cache = std::env::temp_dir().join(format!("jarvis-test-{}", std::process::id()));
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &cache);
        }
        let st = AttachState {
            session_id: Some("abc".into()),
            auto_resume: false,
            cwd: None,
        };
        save_state(&st).unwrap();
        let loaded = load_state().unwrap().unwrap();
        assert_eq!(loaded.session_id, Some("abc".to_string()));
        clear_state().unwrap();
        assert!(load_state().unwrap().is_none());
    }
}
