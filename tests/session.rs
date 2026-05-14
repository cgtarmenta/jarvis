//! Integration tests for the session persistence layer.
//!
//! These exercise the same `session::save` / `session::load_or_new`
//! pair that the pipeline uses, against a tempdir-redirected XDG
//! cache. They're black-box from the library's point of view: write
//! a session through the public API, read the resulting JSON off
//! disk, assert the shape that downstream consumers (and humans
//! running `cat ~/.cache/jarvis/sessions/current.json`) depend on.

use std::fs;

use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;

use jarvis::session::{self, Role, Session};

/// Same XDG redirection helper as `tests/config.rs` uses. Setting
/// env vars is `unsafe` under Rust 2024 because of POSIX setenv's
/// global state; the `#[serial]` attribute on every test makes that
/// safe by keeping them off concurrent threads.
fn redirect_xdg(tmp: &TempDir) {
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
        std::env::set_var("XDG_CACHE_HOME", tmp.path().join("cache"));
    }
}

/// Spec 0009 (orchestrator D) smoke: when the pipeline calls
/// `add_turn_for_worker` + `set_active_worker_session` + `save`
/// against a fresh session, the resulting `current.json` carries
/// every v2 field: `session_schema_version = 2`, the active_workers
/// map with the worker's session id, and per-turn `dispatched_to`
/// plus `worker_session_id`. This is the on-disk contract the
/// pipeline produces today and the dispatcher (hija A) will lean on
/// tomorrow.
#[test]
#[serial]
fn pipeline_write_path_produces_v2_session_json() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);

    let mut sess = Session::new();
    // Mirror what `pipeline::run_turn` does post-agent-call:
    //   add user turn, add assistant turn, update active_workers.
    sess.add_turn_for_worker(
        Role::User,
        "ping prompt".to_string(),
        "claude".to_string(),
        Some("c47a097d-test".to_string()),
    );
    sess.add_turn_for_worker(
        Role::Assistant,
        "pong reply".to_string(),
        "claude".to_string(),
        Some("c47a097d-test".to_string()),
    );
    sess.set_active_worker_session("claude", Some("c47a097d-test".to_string()));

    session::save(&sess).expect("save succeeds");

    let path = session::session_path().expect("session path resolves");
    let raw = fs::read_to_string(&path).expect("session.json readable");
    let parsed: Value = serde_json::from_str(&raw).expect("session.json is valid JSON");

    // v2 schema version.
    assert_eq!(
        parsed.get("session_schema_version").and_then(Value::as_u64),
        Some(2),
        "expected session_schema_version=2, got: {parsed}"
    );

    // active_workers map.
    let workers = parsed
        .get("active_workers")
        .and_then(Value::as_object)
        .expect("active_workers is an object");
    assert_eq!(
        workers.get("claude").and_then(Value::as_str),
        Some("c47a097d-test"),
        "active_workers should map claude → uuid"
    );

    // Per-turn fields.
    let turns = parsed
        .get("turns")
        .and_then(Value::as_array)
        .expect("turns is an array");
    assert_eq!(turns.len(), 2);
    for turn in turns {
        assert_eq!(
            turn.get("dispatched_to").and_then(Value::as_str),
            Some("claude"),
            "every turn should record dispatched_to=claude"
        );
        assert_eq!(
            turn.get("worker_session_id").and_then(Value::as_str),
            Some("c47a097d-test"),
            "every turn should record worker_session_id"
        );
    }
}

/// Spec 0009: a v1 session.json on disk (no schema_version, no
/// active_workers, no dispatched_to on turns) is loaded with the
/// documented defaults, and saving it back upgrades the file to
/// v2 in place. Confirms the in-place migration path the spec
/// promised — users with existing sessions don't lose history,
/// and v1 files quietly become v2 on the next persist.
#[test]
#[serial]
fn v1_session_on_disk_upgrades_to_v2_on_next_save() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);

    let path = session::session_path().expect("session path resolves");

    // Drop a hand-written v1 session at the canonical path.
    let v1_json = r#"{
        "id": "s-legacy",
        "started_at": 1,
        "last_activity": 2,
        "turns": [
            { "role": "user", "content": "hi", "timestamp": 1 },
            { "role": "assistant", "content": "hello", "timestamp": 2 }
        ]
    }"#;
    fs::write(&path, v1_json).expect("write v1 session");

    // Load it; the deserialiser should fill defaults.
    let sess = session::load_or_new(0).expect("load v1");
    assert_eq!(sess.id, "s-legacy");
    assert_eq!(sess.session_schema_version, 1, "loaded as v1");
    for turn in &sess.turns {
        assert_eq!(turn.dispatched_to, "claude");
    }

    // Now save and re-read — the on-disk file should be v2.
    session::save(&sess).expect("save migrates");
    let raw = fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        parsed.get("session_schema_version").and_then(Value::as_u64),
        Some(2),
        "saved file should be v2; got: {parsed}"
    );
    // Backfilled per-turn fields appear in the saved JSON too.
    let turns = parsed.get("turns").and_then(Value::as_array).unwrap();
    for turn in turns {
        assert_eq!(
            turn.get("dispatched_to").and_then(Value::as_str),
            Some("claude")
        );
    }
}
