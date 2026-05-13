//! Conversational session state.
//!
//! Each `pipeline::run_once` turn loads the active session, calls the agent
//! with the accumulated history, and persists the new turns. This lets the
//! agent continue a conversation across wake events — "Mutombo, ¿qué hora
//! es?", *answer*, "Mutombo, ¿y en Tokio?" — without re-stating context.
//!
//! Storage model
//! -------------
//! A single "current" session lives at `$XDG_CACHE_HOME/jarvis/sessions/
//! current.json`. The file is written atomically (write to `.tmp`, rename)
//! so a crash mid-turn can't leave a half-serialised JSON behind.
//!
//! Lifecycle
//! ---------
//! * **New** session is created on first turn after a reset or after the
//!   TTL elapsed since the last activity.
//! * **Reset** happens explicitly (CLI `session reset`, voice phrase) or
//!   automatically when `last_activity` is older than `ttl_seconds`.
//! * **Truncation**: when the in-memory turn list exceeds `max_turns`, the
//!   oldest turns are dropped before the next agent call. Keeps the prompt
//!   token budget bounded for long-running sessions.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Who said this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    /// Label used when embedding the conversation into a free-text prompt
    /// (e.g. for the Claude `--print` flow). Stable across releases — agent
    /// CLIs match on these labels.
    pub fn label(self) -> &'static str {
        match self {
            Role::User => "User",
            Role::Assistant => "Assistant",
        }
    }

    /// Identifier used by the OpenAI / Gemini chat APIs.
    pub fn api_role(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: u64,
    pub last_activity: u64,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

impl Default for Session {
    fn default() -> Self {
        let now = unix_now();
        Self {
            id: format!("s-{now}"),
            started_at: now,
            last_activity: now,
            turns: Vec::new(),
        }
    }
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_expired(&self, ttl_seconds: u64) -> bool {
        // ttl_seconds == 0 disables expiry — useful for very long-running
        // single-day sessions where the user explicitly resets when done.
        ttl_seconds > 0 && unix_now().saturating_sub(self.last_activity) > ttl_seconds
    }

    pub fn add_turn(&mut self, role: Role, content: String) {
        let ts = unix_now();
        self.last_activity = ts;
        self.turns.push(Turn {
            role,
            content,
            timestamp: ts,
        });
    }

    /// Drop oldest turns until at most `max_turns` remain. A `max_turns`
    /// of 0 means "no history at all" (effectively stateless mode).
    pub fn truncate_to(&mut self, max_turns: usize) {
        if self.turns.len() > max_turns {
            let drop = self.turns.len() - max_turns;
            self.turns.drain(0..drop);
        }
    }
}

pub fn session_path() -> Result<PathBuf> {
    let dir = crate::config::cache_dir()?.join("sessions");
    fs::create_dir_all(&dir).with_context(|| format!("creating session dir: {}", dir.display()))?;
    Ok(dir.join("current.json"))
}

/// Read the current session from disk. Returns a fresh empty `Session`
/// when:
///   * no file exists yet;
///   * the file is corrupt (so we never *panic* over bad state);
///   * the TTL has elapsed since the last activity.
pub fn load_or_new(ttl_seconds: u64) -> Result<Session> {
    let path = session_path()?;
    if !path.exists() {
        return Ok(Session::new());
    }
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            info!(error = %e, "session file unreadable; starting fresh");
            return Ok(Session::new());
        }
    };
    let session: Session = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            info!(error = %e, "session file corrupt; starting fresh");
            return Ok(Session::new());
        }
    };
    if session.is_expired(ttl_seconds) {
        info!(
            id = %session.id,
            idle_seconds = unix_now().saturating_sub(session.last_activity),
            "session expired; starting fresh"
        );
        return Ok(Session::new());
    }
    Ok(session)
}

/// Atomic write — temp file then rename, so a crash mid-write doesn't
/// leave a truncated session.json behind.
pub fn save(session: &Session) -> Result<()> {
    let path = session_path()?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(session).context("serialising session")?;
    fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Forget the current session entirely. The next `load_or_new` will return
/// a brand-new empty Session.
pub fn reset() -> Result<()> {
    let path = session_path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing session: {}", path.display()))?;
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_turn_updates_last_activity() {
        let mut s = Session::new();
        let before = s.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(1100));
        s.add_turn(Role::User, "hello".into());
        assert!(s.last_activity > before, "last_activity should advance");
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].role, Role::User);
    }

    #[test]
    fn truncate_drops_oldest() {
        let mut s = Session::new();
        for i in 0..10 {
            s.add_turn(Role::User, format!("turn {i}"));
        }
        s.truncate_to(3);
        assert_eq!(s.turns.len(), 3);
        assert_eq!(s.turns[0].content, "turn 7");
        assert_eq!(s.turns[2].content, "turn 9");
    }

    #[test]
    fn ttl_zero_disables_expiry() {
        let s = Session {
            last_activity: 0,
            ..Session::new()
        };
        assert!(!s.is_expired(0));
    }
}
