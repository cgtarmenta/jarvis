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

/// Spec 0011 (orchestrator E1-4): `jarvis task list` finds tasks
/// in the cache dir, separates active from terminal via the
/// `--all` flag, and `task show` resolves an id prefix to print
/// the full record. We seed a v2 task record into the tempdir
/// cache and assert the CLI output names the right pieces.
#[test]
#[serial]
fn task_list_and_show_render_records() {
    use std::fs;
    let tmp = TempDir::new().unwrap();
    let tasks_dir = tmp.path().join("cache").join("jarvis").join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    let task_id = "t-1715700000-aabbcc";
    let task_dir = tasks_dir.join(task_id);
    fs::create_dir_all(&task_dir).unwrap();
    fs::write(
        task_dir.join("record.json"),
        r#"{
            "id": "t-1715700000-aabbcc",
            "thread_id": "s-test",
            "worker_id": "gemini",
            "spawn_time": 1715700000,
            "completion_time": 1715700060,
            "status": "completed",
            "user_intent": "analyze syslog and summarise errors",
            "command": ["gemini-cli", "--prompt", "analyze syslog"],
            "pid": null,
            "exit_code": 0,
            "summary": "found 3 errors at startup"
        }"#,
    )
    .unwrap();
    fs::write(task_dir.join("stdout.txt"), "found 3 errors at startup").unwrap();
    fs::write(task_dir.join("stderr.txt"), "").unwrap();

    // task list (no --all) hides the completed task.
    redirect_xdg(jarvis().args(["task", "list"]), &tmp)
        .assert()
        .success()
        .stdout(contains("Total: 1"))
        .stdout(contains("no active tasks"));

    // task list --all surfaces it.
    redirect_xdg(jarvis().args(["task", "list", "--all"]), &tmp)
        .assert()
        .success()
        .stdout(contains("aabbcc"))
        .stdout(contains("gemini"))
        .stdout(contains("completed"));

    // task show by prefix.
    redirect_xdg(jarvis().args(["task", "show", "t-1715700000"]), &tmp)
        .assert()
        .success()
        .stdout(contains("worker:"))
        .stdout(contains("status:"))
        .stdout(contains("Completed"))
        .stdout(contains("analyze syslog"))
        .stdout(contains("summary:"))
        .stdout(contains("found 3 errors"))
        .stdout(contains("stdout.txt"));

    // task show with a non-matching prefix errors.
    redirect_xdg(jarvis().args(["task", "show", "t-9999"]), &tmp)
        .assert()
        .failure();
}

/// Spec 0009 (orchestrator D-3): `jarvis session show` renders the
/// new v2 fields — `session_schema_version`, the `active_workers`
/// map, and per-turn `dispatched_to` — when a v2 session is on
/// disk. We seed a tempdir cache with a hand-crafted v2 session
/// JSON and assert the CLI output names the right pieces.
#[test]
#[serial]
fn session_show_renders_v2_fields() {
    use std::fs;
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("cache").join("jarvis").join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let v2 = r#"{
        "id": "s-test",
        "started_at": 100,
        "last_activity": 200,
        "session_schema_version": 2,
        "active_workers": { "claude": "uuid-abc", "time": null },
        "turns": [
            {
                "role": "user",
                "content": "hola",
                "timestamp": 150,
                "dispatched_to": "claude",
                "worker_session_id": "uuid-abc"
            }
        ]
    }"#;
    fs::write(sessions_dir.join("current.json"), v2).unwrap();

    redirect_xdg(jarvis().args(["session", "show"]), &tmp)
        .assert()
        .success()
        .stdout(contains("schema:        v2"))
        .stdout(contains("active_workers:"))
        .stdout(contains("claude"))
        .stdout(contains("uuid-abc"))
        .stdout(contains("time"))
        .stdout(contains("(stateless)"))
        // `[User → claude]` for the per-turn dispatched_to surface.
        .stdout(contains("→ claude]"));
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
