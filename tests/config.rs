//! Integration tests for the config module.
//!
//! These tests do not touch the user's real `~/.config/jarvis` — they use
//! temp dirs and override XDG via `XDG_CONFIG_HOME` / `XDG_DATA_HOME` /
//! `XDG_CACHE_HOME`. Because `directories` reads the env once per call we
//! mark them `serial_test::serial` so they can't race each other.

use jarvis::config::{self, JarvisConfig};
use serial_test::serial;
use tempfile::TempDir;

/// Point all the XDG dirs at a fresh tempdir so we don't clobber real config.
/// Setting env vars is `unsafe` in Rust 2024 because of POSIX threading; the
/// tests are serial so this is fine.
fn redirect_xdg(tmp: &TempDir) {
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
        std::env::set_var("XDG_CACHE_HOME", tmp.path().join("cache"));
        std::env::remove_var("JARVIS_AGENT");
    }
}

#[test]
#[serial]
fn ensure_config_creates_file() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let path = config::ensure_config().expect("ensure_config");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("[agent]"));
}

#[test]
#[serial]
fn load_returns_defaults_for_example() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let path = config::ensure_config().unwrap();
    let cfg = config::load(&path).expect("parse example");
    assert_eq!(cfg.wake.backend, "none");
    assert_eq!(cfg.wake.phrases, vec!["jarvis".to_string()]);
    assert!(cfg.speak_responses);
    assert_eq!(cfg.agent.name, "claude");
}

#[test]
#[serial]
fn env_overrides_agent() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    unsafe {
        std::env::set_var("JARVIS_AGENT", "openai");
    }
    let path = config::ensure_config().unwrap();
    let cfg = config::load(&path).unwrap();
    assert_eq!(cfg.agent.name, "openai");
}

/// Spec 0015 — `[apps.aliases]` round-trips through `config::load`.
/// User entries are kept verbatim; the handler does case-folding /
/// normalisation at construction, not at parse time, so the raw
/// keys survive.
#[test]
#[serial]
fn apps_aliases_round_trip() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let path = config::ensure_config().unwrap();

    // Append an [apps.aliases] block to the example so config::load
    // sees it. We *append* rather than rewrite because the wizard's
    // serializer (spec 0014 + 0015) is exercised in a separate
    // integration suite; this test pins config::load's deserialisation.
    let mut content = std::fs::read_to_string(&path).unwrap();
    content.push_str("\n[apps.aliases]\n");
    content.push_str("\"signal-desktop\" = \"signal\"\n");
    content.push_str("\"navegador\" = \"firefox\"\n");
    std::fs::write(&path, content).unwrap();

    let cfg = config::load(&path).expect("parse with apps.aliases");
    assert_eq!(cfg.apps.aliases.len(), 2);
    assert_eq!(
        cfg.apps.aliases.get("signal-desktop").map(|s| s.as_str()),
        Some("signal")
    );
    assert_eq!(
        cfg.apps.aliases.get("navegador").map(|s| s.as_str()),
        Some("firefox")
    );
}

/// Default `JarvisConfig` has an empty `apps.aliases` map — no
/// surprises at first-run, and the handler degrades to its
/// built-in alias table.
#[test]
fn apps_aliases_default_is_empty() {
    let cfg = JarvisConfig::default();
    assert!(cfg.apps.aliases.is_empty());
}

#[test]
fn defaults_are_sensible() {
    let cfg = JarvisConfig::default();
    assert!(cfg.speak_responses);
    assert_eq!(cfg.stt.backend, "whisper-cli");
    assert_eq!(cfg.tts.backend, "piper");
    assert_eq!(cfg.record.backend, "auto");
    assert!(!cfg.wake.enabled);
}

/// Spec 0007: the follow-up window must default to a value generous
/// enough that a user thinking briefly between turns doesn't time
/// out. Live testing on 2026-05-13 showed 6 s was too tight (the
/// user routinely took ~10-13 s to start the next turn), so the
/// default was bumped to 10.0. The contract is "at least 8 s" so
/// modest future tuning doesn't trip this test.
#[test]
fn followup_window_default_is_generous() {
    let cfg = JarvisConfig::default();
    assert!(
        cfg.session.followup_window_secs >= 8.0,
        "expected followup_window_secs default >= 8.0, got {}",
        cfg.session.followup_window_secs
    );
}

/// Spec 0007: setting `followup_window_secs = 0` in TOML must disable
/// the follow-up loop. We round-trip a minimal config that includes
/// the field and check it lands on the deserialised struct unchanged.
#[test]
#[serial]
fn followup_window_zero_is_preserved() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let cfg_path = tmp.path().join("config").join("jarvis").join("config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg_path,
        "config_version = 2\n\
         log_level = \"INFO\"\n\
         [session]\n\
         followup_window_secs = 0.0\n",
    )
    .unwrap();
    let cfg = config::load(&cfg_path).expect("parse followup=0");
    assert_eq!(cfg.session.followup_window_secs, 0.0);
}

/// Spec 0007: a custom non-default window value round-trips intact.
/// This catches schema typos (wrong field name, wrong type) that the
/// `deny_unknown_fields` parse would surface as errors but that a
/// missing default fallback could silently mask.
#[test]
#[serial]
fn followup_window_custom_value_round_trips() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let cfg_path = tmp.path().join("config").join("jarvis").join("config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg_path,
        "config_version = 2\n\
         log_level = \"INFO\"\n\
         [session]\n\
         followup_window_secs = 12.5\n",
    )
    .unwrap();
    let cfg = config::load(&cfg_path).expect("parse followup=12.5");
    assert!(
        (cfg.session.followup_window_secs - 12.5).abs() < f32::EPSILON,
        "expected followup_window_secs 12.5, got {}",
        cfg.session.followup_window_secs
    );
}

#[test]
#[serial]
fn old_schema_config_fails_with_migration_hint() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let cfg_path = tmp.path().join("config").join("jarvis").join("config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    // Write a v0/legacy config with the now-removed `model` wake field.
    std::fs::write(
        &cfg_path,
        "log_level = \"INFO\"\nspeak_responses = true\n\n[wake]\nmodel = \"hey_jarvis\"\n",
    )
    .unwrap();

    let err = config::load(&cfg_path).expect_err("expected version migration error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("config_version") && msg.contains("jarvis setup"),
        "expected migration hint mentioning config_version and jarvis setup, got: {msg}"
    );
}

#[test]
#[serial]
fn future_schema_config_fails_explicitly() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(&tmp);
    let cfg_path = tmp.path().join("config").join("jarvis").join("config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(&cfg_path, "config_version = 999\nlog_level = \"INFO\"\n").unwrap();

    let err = config::load(&cfg_path).expect_err("expected future-version refusal");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("999") && msg.contains("newer"),
        "expected 'newer than this binary' refusal, got: {msg}"
    );
}
