//! Integration tests for spec 0014 — setup wizard step for
//! `[dispatcher.fallback]`.
//!
//! We can't drive the wizard's interactive prompts from a test (it
//! reads from a real TTY through dialoguer), so these tests pin the
//! non-interactive contract pieces:
//!
//! - The URL validator's accept/reject set.
//! - That `save_config`'s output now includes `session`, `tasks`, and
//!   `[dispatcher.fallback]` — the regression fix that prevents
//!   re-running `jarvis setup` from silently wiping a configured
//!   stage-2 backend.
//! - That a wizard-shaped `[dispatcher.fallback]` value round-trips
//!   through `config::load` and survives as an equivalent
//!   `JarvisConfig`.
//! - That the load-failure hint trigger fires on the right substring
//!   and not on superficially-similar ones.

use jarvis::config::{self, JarvisConfig};
use jarvis::setup;

/// Cheap http(s):// heuristic accepts what the spec lists and rejects
/// the obvious scheme mismatches + empties.
#[test]
fn url_validator_accepts_documented_schemes() {
    assert!(setup::looks_like_http_url(
        "http://localhost:11434/v1/chat/completions"
    ));
    assert!(setup::looks_like_http_url(
        "https://api.groq.com/openai/v1/chat/completions"
    ));
    assert!(setup::looks_like_http_url(
        "https://triton.local/v2/models/llama/infer"
    ));
}

/// Whitespace trimming kicks in across the wizard's text fields. The
/// regression we caught manually: pasting `' qwen-3.6-plus-fireworks'`
/// (leading space from a copy off oz's pretty table) made the live
/// probe fail because oz rejects the id verbatim. Trimming
/// normalises this before it ever leaves the wizard.
#[test]
fn normalize_user_input_strips_surrounding_whitespace() {
    assert_eq!(
        setup::normalize_user_input("  qwen-3.6-plus-fireworks  "),
        "qwen-3.6-plus-fireworks"
    );
    assert_eq!(
        setup::normalize_user_input("\n\thttps://api.example.com/v1\t \n"),
        "https://api.example.com/v1"
    );
    assert_eq!(setup::normalize_user_input(""), "");
    assert_eq!(
        setup::normalize_user_input("no-whitespace"),
        "no-whitespace"
    );
}

/// And the URL validator must agree with the trimmed shape — a value
/// that's only valid after trimming still has to pass when the
/// wizard's validator pipes it through `normalize_user_input`.
#[test]
fn url_validator_accepts_trimmed_input() {
    let raw = "   https://api.groq.com/openai/v1/chat/completions   ";
    let trimmed = setup::normalize_user_input(raw);
    assert!(setup::looks_like_http_url(&trimmed));
}

#[test]
fn url_validator_rejects_empty_and_non_http() {
    assert!(!setup::looks_like_http_url(""));
    assert!(!setup::looks_like_http_url("api.groq.com/chat"));
    assert!(!setup::looks_like_http_url("ftp://example.com"));
    assert!(!setup::looks_like_http_url("http://"));
    assert!(!setup::looks_like_http_url("https:///path"));
}

/// `render_config` now emits `[session]`, `[tasks]`, and
/// `[dispatcher.fallback]` when populated. Before spec 0014 the
/// wizard's save dropped those sections silently, which would have
/// wiped any user-edited stage-2 config on every `jarvis setup`.
#[test]
fn render_emits_session_tasks_and_dispatcher_sections() {
    let mut cfg = JarvisConfig::default();
    let mut t = toml::Table::new();
    t.insert(
        "backend".into(),
        toml::Value::String("openai_compat".into()),
    );
    t.insert(
        "endpoint".into(),
        toml::Value::String("https://example.com/v1/chat/completions".into()),
    );
    t.insert("model".into(), toml::Value::String("test-model".into()));
    t.insert("timeout_secs".into(), toml::Value::Integer(5));
    cfg.dispatcher.fallback = Some(toml::Value::Table(t));

    let rendered = setup::render_config(&cfg).expect("render");
    assert!(rendered.contains("[session]"), "session header missing");
    assert!(rendered.contains("[tasks]"), "tasks header missing");
    assert!(
        rendered.contains("[dispatcher.fallback]"),
        "dispatcher.fallback header missing — regression: wizard would wipe stage-2 config on resave"
    );
    assert!(rendered.contains("backend = \"openai_compat\""));
    assert!(rendered.contains("model = \"test-model\""));
}

/// A wizard-shaped openai_compat block survives a full
/// render → write → load round trip. The reloaded `JarvisConfig`
/// holds an equivalent `dispatcher.fallback` value, and
/// `dispatcher::llm::build_llm_stage` can consume it — i.e. what we
/// wrote during setup is exactly what the daemon will accept on
/// startup.
#[test]
fn openai_compat_block_round_trips_through_config_load() {
    let mut cfg = JarvisConfig::default();
    let mut t = toml::Table::new();
    t.insert(
        "backend".into(),
        toml::Value::String("openai_compat".into()),
    );
    t.insert(
        "endpoint".into(),
        toml::Value::String("https://api.groq.com/openai/v1/chat/completions".into()),
    );
    t.insert(
        "model".into(),
        toml::Value::String("llama-3.1-8b-instant".into()),
    );
    t.insert("api_key".into(), toml::Value::String("sk-redacted".into()));
    t.insert("timeout_secs".into(), toml::Value::Integer(7));
    cfg.dispatcher.fallback = Some(toml::Value::Table(t.clone()));

    let rendered = setup::render_config(&cfg).expect("render");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, rendered).unwrap();

    let reloaded = config::load(&path).expect("config::load");
    let fb = reloaded
        .dispatcher
        .fallback
        .as_ref()
        .expect("fallback survived round trip");
    let reloaded_table = fb.as_table().expect("fallback is a table");
    assert_eq!(
        reloaded_table.get("backend").and_then(|v| v.as_str()),
        Some("openai_compat")
    );
    assert_eq!(
        reloaded_table.get("endpoint").and_then(|v| v.as_str()),
        Some("https://api.groq.com/openai/v1/chat/completions")
    );
    assert_eq!(
        reloaded_table.get("model").and_then(|v| v.as_str()),
        Some("llama-3.1-8b-instant")
    );
    assert_eq!(
        reloaded_table.get("api_key").and_then(|v| v.as_str()),
        Some("sk-redacted")
    );
    assert_eq!(
        reloaded_table
            .get("timeout_secs")
            .and_then(|v| v.as_integer()),
        Some(7)
    );

    // The reloaded value must also be acceptable to the live stage-2
    // builder — the whole point of the wizard producing this shape.
    jarvis::dispatcher::llm::build_llm_stage(fb).expect("build_llm_stage accepts wizard output");
}

/// Same round-trip for the oz backend: minimal shape (backend + model)
/// loads back and `build_llm_stage` accepts it.
#[test]
fn oz_block_round_trips_through_config_load() {
    let mut cfg = JarvisConfig::default();
    let mut t = toml::Table::new();
    t.insert("backend".into(), toml::Value::String("oz".into()));
    t.insert(
        "model".into(),
        toml::Value::String("claude-3.7-sonnet".into()),
    );
    cfg.dispatcher.fallback = Some(toml::Value::Table(t));

    let rendered = setup::render_config(&cfg).expect("render");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, rendered).unwrap();

    let reloaded = config::load(&path).expect("config::load");
    let fb = reloaded.dispatcher.fallback.expect("fallback present");
    let table = fb.as_table().unwrap();
    assert_eq!(table.get("backend").and_then(|v| v.as_str()), Some("oz"));
    assert_eq!(
        table.get("model").and_then(|v| v.as_str()),
        Some("claude-3.7-sonnet")
    );
    jarvis::dispatcher::llm::build_llm_stage(&toml::Value::Table(table.clone()))
        .expect("build_llm_stage accepts oz wizard output");
}

/// A configured `headers` sub-table emerges as
/// `[dispatcher.fallback.headers]` — the dotted parent header is
/// what makes the round trip work for the openai_compat power-user
/// surface. Locking this because a naive serializer would emit
/// `[headers]` (relative) and break reload.
#[test]
fn dispatcher_fallback_renders_nested_headers_with_full_prefix() {
    let mut cfg = JarvisConfig::default();
    let mut t = toml::Table::new();
    t.insert(
        "backend".into(),
        toml::Value::String("openai_compat".into()),
    );
    t.insert(
        "endpoint".into(),
        toml::Value::String("https://example.com/v1/chat/completions".into()),
    );
    t.insert("model".into(), toml::Value::String("m".into()));
    let mut headers = toml::Table::new();
    headers.insert(
        "X-VPN-Route".into(),
        toml::Value::String("gb200-cluster".into()),
    );
    t.insert("headers".into(), toml::Value::Table(headers));
    cfg.dispatcher.fallback = Some(toml::Value::Table(t));

    let rendered = setup::render_config(&cfg).expect("render");
    assert!(
        rendered.contains("[dispatcher.fallback.headers]"),
        "expected full-path header section in:\n{rendered}"
    );

    // And it must reload cleanly through the full pipeline.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, rendered).unwrap();
    let reloaded = config::load(&path).expect("config::load");
    let fb = reloaded.dispatcher.fallback.unwrap();
    jarvis::dispatcher::llm::build_llm_stage(&fb)
        .expect("build_llm_stage accepts nested-headers wizard output");
}

/// Skip path: `cfg.dispatcher.fallback = None` produces a rendered
/// file with no `[dispatcher.fallback]` section. Important because
/// the wizard's "stage 2 stays disabled" branch must result in a
/// file the daemon happily loads without stage 2 wired in.
#[test]
fn no_dispatcher_section_when_fallback_is_none() {
    let cfg = JarvisConfig::default();
    assert!(cfg.dispatcher.fallback.is_none());
    let rendered = setup::render_config(&cfg).expect("render");
    assert!(
        !rendered.contains("[dispatcher.fallback]"),
        "no fallback configured but section emitted:\n{rendered}"
    );

    // Reload still succeeds and produces `fallback: None`.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, rendered).unwrap();
    let reloaded = config::load(&path).expect("config::load");
    assert!(reloaded.dispatcher.fallback.is_none());
}

/// Load-failure hint trigger: the substring check returns true for
/// the canonical section header (with or without sub-keys) and
/// false for unrelated text that mentions the field name without
/// the bracket prefix.
#[test]
fn load_failure_hint_triggers_on_section_header_only() {
    assert!(setup::config_text_mentions_dispatcher_fallback(
        "[dispatcher.fallback]\nbackend = \"oz\"\nmodel = \"x\"\n"
    ));
    assert!(setup::config_text_mentions_dispatcher_fallback(
        "[dispatcher.fallback.headers]\n\"X-Foo\" = \"bar\"\n"
    ));
    // Free-text mention without the bracket should NOT trigger — a
    // commented-out example or a docstring talking about the field
    // doesn't mean the user had a working block.
    assert!(!setup::config_text_mentions_dispatcher_fallback(
        "# see docs for dispatcher.fallback\n"
    ));
    assert!(!setup::config_text_mentions_dispatcher_fallback(""));
}
