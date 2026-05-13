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

#[test]
fn defaults_are_sensible() {
    let cfg = JarvisConfig::default();
    assert!(cfg.speak_responses);
    assert_eq!(cfg.stt.backend, "whisper-cli");
    assert_eq!(cfg.tts.backend, "piper");
    assert_eq!(cfg.record.backend, "auto");
    assert!(!cfg.wake.enabled);
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
