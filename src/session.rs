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

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

/// The schema version this binary writes. Bumped when the on-disk
/// session JSON gains required fields that old binaries don't know
/// about. See spec 0009 for the v1 → v2 migration that introduced
/// `dispatched_to` / `worker_session_id` on turns and the
/// `active_workers` map on sessions.
pub const CURRENT_SESSION_SCHEMA_VERSION: u32 = 2;

/// The pseudo-id used as `dispatched_to` for turns loaded from a v1
/// session file that didn't record which worker handled them. v1
/// shipped with only one agent (Claude), so "claude" is the only
/// reasonable backfill — same string the bundled `workers/claude.toml`
/// manifest uses.
const V1_DEFAULT_DISPATCHED_TO: &str = "claude";

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
    /// Worker id that handled this turn. v1 sessions get backfilled
    /// to `"claude"` because that was the only worker. New turns
    /// always carry the dispatched worker's id verbatim.
    #[serde(default = "default_dispatched_to")]
    pub dispatched_to: String,
    /// The worker's own session id at the time of this turn, when
    /// applicable. Stateless workers (time, calc, etc.) always
    /// write `None`; stateful workers record whatever id the
    /// dispatcher captured via `session_id_capture` or sourced from
    /// `active_workers` (per spec D).
    #[serde(default)]
    pub worker_session_id: Option<String>,
}

fn default_dispatched_to() -> String {
    V1_DEFAULT_DISPATCHED_TO.to_string()
}

fn default_schema_version() -> u32 {
    // Missing field on disk means v1 — files written by binaries that
    // predate spec 0009 (orchestrator D).
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: u64,
    pub last_activity: u64,
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Schema version on disk. Loaded as the legacy `1` when absent
    /// (v1 sessions written before spec 0009 didn't carry a version
    /// field). Always serialised as `CURRENT_SESSION_SCHEMA_VERSION`
    /// — `save()` migrates in-place by writing the new constant
    /// regardless of what came off disk.
    #[serde(default = "default_schema_version")]
    pub session_schema_version: u32,
    /// Map from worker id to that worker's most recently-known
    /// session id for this thread. Built up over time by the
    /// dispatcher (spec D + hija A): every stateful worker that
    /// handles a turn writes its session id here, and follow-up
    /// dispatches to the same worker resume that session.
    ///
    /// Empty on freshly-loaded v1 sessions; the dispatcher populates
    /// it lazily as workers are invoked. Stateless workers leave the
    /// map untouched.
    #[serde(default)]
    pub active_workers: HashMap<String, Option<String>>,
}

impl Default for Session {
    fn default() -> Self {
        let now = unix_now();
        Self {
            id: format!("s-{now}"),
            started_at: now,
            last_activity: now,
            turns: Vec::new(),
            session_schema_version: CURRENT_SESSION_SCHEMA_VERSION,
            active_workers: HashMap::new(),
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

    /// Append a turn that was dispatched to `"claude"` with no
    /// captured worker session id. Kept for code paths predating
    /// spec D's pipeline integration (the existing
    /// `pipeline::run_once` calls this); prefer
    /// [`add_turn_for_worker`](Self::add_turn_for_worker) in new
    /// callers so the dispatch metadata is recorded honestly.
    pub fn add_turn(&mut self, role: Role, content: String) {
        self.add_turn_for_worker(role, content, V1_DEFAULT_DISPATCHED_TO.to_string(), None);
    }

    /// Append a turn with full dispatch metadata.
    ///
    /// `dispatched_to` is the id of the worker that handled this turn
    /// (matches the manifest id or built-in handler id);
    /// `worker_session_id` is the worker's own session id at that
    /// moment (whatever `session_id_capture` produced, or whatever
    /// id the dispatcher resumed from). Stateless workers should
    /// pass `None`.
    pub fn add_turn_for_worker(
        &mut self,
        role: Role,
        content: String,
        dispatched_to: String,
        worker_session_id: Option<String>,
    ) {
        let ts = unix_now();
        self.last_activity = ts;
        self.turns.push(Turn {
            role,
            content,
            timestamp: ts,
            dispatched_to,
            worker_session_id,
        });
    }

    /// Record (or clear) the active session id for a worker on this
    /// thread. Called by the dispatcher after a successful turn:
    /// stateful workers that produced a captured session id update
    /// their entry; stateless workers don't call this. Returns the
    /// previous value, mostly for tests and log lines.
    pub fn set_active_worker_session(
        &mut self,
        worker_id: impl Into<String>,
        session_id: Option<String>,
    ) -> Option<Option<String>> {
        self.active_workers.insert(worker_id.into(), session_id)
    }

    /// Look up the active session id for a worker. Returns `None`
    /// when the worker has never been invoked on this thread.
    /// `Some(None)` means "invoked but no session id captured"
    /// (a stateless worker, or one whose capture rule never matched).
    pub fn active_worker_session(&self, worker_id: &str) -> Option<&Option<String>> {
        self.active_workers.get(worker_id)
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
///
/// Always serialises `session_schema_version =
/// CURRENT_SESSION_SCHEMA_VERSION`. Callers can pass a `Session` they
/// loaded from a v1 file (whose `session_schema_version` was
/// backfilled to `1` by the deserialiser); this function silently
/// upgrades it on the next save. That's the migration path — no
/// separate "migrate" pass needed.
pub fn save(session: &Session) -> Result<()> {
    let path = session_path()?;
    let tmp = path.with_extension("json.tmp");
    let migrated = Session {
        session_schema_version: CURRENT_SESSION_SCHEMA_VERSION,
        ..session.clone()
    };
    let json = serde_json::to_string_pretty(&migrated).context("serialising session")?;
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

    /// Spec 0009: a v1 session.json (no schema_version, no
    /// dispatched_to on turns, no active_workers on the session)
    /// loads cleanly. The defaults backfill: dispatched_to becomes
    /// "claude" on every turn, active_workers is empty, and the
    /// session reads as schema_version=1 so we know it's legacy.
    #[test]
    fn v1_session_json_loads_with_defaults() {
        let v1_json = r#"{
            "id": "s-12345",
            "started_at": 0,
            "last_activity": 0,
            "turns": [
                { "role": "user", "content": "hi", "timestamp": 1 },
                { "role": "assistant", "content": "hello", "timestamp": 2 }
            ]
        }"#;
        let s: Session = serde_json::from_str(v1_json).expect("v1 session parses");
        assert_eq!(s.id, "s-12345");
        assert_eq!(s.session_schema_version, 1, "missing field → v1");
        assert!(s.active_workers.is_empty(), "no active_workers in v1");
        assert_eq!(s.turns.len(), 2);
        for turn in &s.turns {
            assert_eq!(
                turn.dispatched_to, "claude",
                "v1 turn dispatched_to should backfill to claude"
            );
            assert!(
                turn.worker_session_id.is_none(),
                "v1 turn has no worker session id"
            );
        }
    }

    /// Spec 0009: a v2 session.json (all fields present) loads
    /// without coercion. Round-trips intact when serialised back.
    #[test]
    fn v2_session_json_roundtrips() {
        let v2_json = r#"{
            "id": "s-22222",
            "started_at": 100,
            "last_activity": 200,
            "session_schema_version": 2,
            "active_workers": { "claude": "uuid-1", "time": null },
            "turns": [
                {
                    "role": "user",
                    "content": "hola",
                    "timestamp": 150,
                    "dispatched_to": "claude",
                    "worker_session_id": "uuid-1"
                }
            ]
        }"#;
        let s: Session = serde_json::from_str(v2_json).expect("v2 parses");
        assert_eq!(s.session_schema_version, 2);
        assert_eq!(s.active_workers.len(), 2);
        assert_eq!(
            s.active_workers.get("claude").and_then(|v| v.as_deref()),
            Some("uuid-1")
        );
        assert!(s.active_workers.contains_key("time"));
        assert!(s.active_workers["time"].is_none());
        assert_eq!(s.turns[0].dispatched_to, "claude");
        assert_eq!(s.turns[0].worker_session_id.as_deref(), Some("uuid-1"));

        // Re-serialise and parse back; must still be a valid v2.
        let again = serde_json::to_string(&s).expect("serialise");
        let s2: Session = serde_json::from_str(&again).expect("roundtrip");
        assert_eq!(s2.session_schema_version, 2);
        assert_eq!(s2.active_workers.len(), 2);
    }

    /// Spec 0009: `add_turn_for_worker` records full dispatch
    /// metadata; the legacy `add_turn` is a compat wrapper that
    /// fills `dispatched_to = "claude"` to preserve current
    /// pipeline behaviour until D-2 swaps it for a worker-aware
    /// call.
    #[test]
    fn add_turn_for_worker_records_metadata() {
        let mut s = Session::new();
        s.add_turn_for_worker(
            Role::User,
            "hi".into(),
            "gemini".into(),
            Some("gem-abc".into()),
        );
        assert_eq!(s.turns[0].dispatched_to, "gemini");
        assert_eq!(s.turns[0].worker_session_id.as_deref(), Some("gem-abc"));

        s.add_turn(Role::Assistant, "hello".into());
        assert_eq!(
            s.turns[1].dispatched_to, "claude",
            "legacy add_turn defaults to claude"
        );
        assert!(s.turns[1].worker_session_id.is_none());
    }

    /// Spec 0009: `active_workers` map operations.
    /// `set_active_worker_session` records the worker's current
    /// session id; `active_worker_session` reads it back; updates
    /// overwrite the previous value and return it.
    #[test]
    fn active_workers_set_and_get() {
        let mut s = Session::new();
        assert!(s.active_worker_session("claude").is_none());

        let prev = s.set_active_worker_session("claude", Some("uuid-1".into()));
        assert!(prev.is_none(), "first set returns None");

        assert_eq!(
            s.active_worker_session("claude")
                .and_then(|opt| opt.as_deref()),
            Some("uuid-1")
        );

        // Updating returns the previous Option<String>.
        let prev = s.set_active_worker_session("claude", Some("uuid-2".into()));
        assert_eq!(prev, Some(Some("uuid-1".into())));

        // Setting to None records "invoked but stateless" — kept as
        // a distinct state from "never invoked".
        let prev = s.set_active_worker_session("time", None);
        assert!(prev.is_none());
        assert!(matches!(s.active_worker_session("time"), Some(None)));
    }

    /// Spec 0009: a saved v1 session migrates to v2 on disk.
    /// We can't easily test `save()` directly without `XDG_*`
    /// redirection (which the integration tests handle), so this
    /// asserts the migration path in isolation: take a Session
    /// with version=1 (as if loaded from disk), serialise via the
    /// shape `save()` uses, confirm the output reads back as v2.
    #[test]
    fn save_migrates_v1_session_to_v2() {
        let s = Session {
            id: "s-legacy".into(),
            started_at: 0,
            last_activity: 0,
            turns: vec![],
            session_schema_version: 1,
            active_workers: HashMap::new(),
        };
        let migrated = Session {
            session_schema_version: CURRENT_SESSION_SCHEMA_VERSION,
            ..s.clone()
        };
        let json = serde_json::to_string(&migrated).unwrap();
        let s2: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s2.session_schema_version, 2);
        assert_eq!(s2.id, "s-legacy");
    }

    /// Spec 0009: truncation drops oldest turns but leaves
    /// `active_workers` intact. The map exists outside the turn
    /// list; dropping turns can't invalidate session-id state for
    /// any worker.
    #[test]
    fn truncate_does_not_touch_active_workers() {
        let mut s = Session::new();
        s.set_active_worker_session("claude", Some("uuid-1".into()));
        for i in 0..5 {
            s.add_turn(Role::User, format!("t{i}"));
        }
        s.truncate_to(2);
        assert_eq!(s.turns.len(), 2);
        assert_eq!(
            s.active_worker_session("claude")
                .and_then(|opt| opt.as_deref()),
            Some("uuid-1")
        );
    }
}
