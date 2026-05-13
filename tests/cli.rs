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
