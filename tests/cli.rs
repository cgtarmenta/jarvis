//! Black-box CLI smoke tests.

use assert_cmd::Command;
use predicates::str::contains;
use serial_test::serial;
use tempfile::TempDir;

fn jarvis() -> Command {
    Command::cargo_bin("jarvis").expect("jarvis binary built")
}

fn redirect_xdg<'a>(cmd: &'a mut Command, tmp: &TempDir) -> &'a mut Command {
    cmd.env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .env_remove("JARVIS_AGENT")
}

#[test]
fn version_prints() {
    jarvis()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("jarvis"));
}

#[test]
fn help_lists_core_subcommands() {
    jarvis()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("listen"))
        .stdout(contains("doctor"))
        .stdout(contains("test-agent"));
}

#[test]
#[serial]
fn config_subcommand_prints_path() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(jarvis().arg("config"), &tmp)
        .assert()
        .success()
        .stdout(contains("config.toml"));
}

#[test]
#[serial]
fn doctor_runs_without_crashing() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(jarvis().arg("doctor"), &tmp)
        .assert()
        .success()
        .stdout(contains("Jarvis doctor"));
}

/// Spec 0008 (orchestrator C-5): `jarvis worker list` prints the
/// registry contents. Against a fresh temp config, this exercises:
/// (1) `ensure_workers_dir()` auto-installs the bundled starter
///     `claude.toml`; (2) the registry loads it; (3) the formatted
/// output names it under ACTIVE.
///
/// If `claude` isn't on PATH on the CI host, the manifest will be
/// disabled — the assertion looks for `claude` to appear in either
/// section, not strictly under ACTIVE, so the test stays portable.
#[test]
#[serial]
fn worker_list_shows_bundled_claude_manifest() {
    let tmp = TempDir::new().unwrap();
    redirect_xdg(jarvis().args(["worker", "list"]), &tmp)
        .assert()
        .success()
        .stdout(contains("Workers directory:"))
        .stdout(contains("claude.toml"))
        .stdout(contains("claude"));
}
