//! Async task spawn + supervisor + completion notification.
//!
//! `spawn_async_task` forks the worker as a detached child of the
//! daemon, captures stdout/stderr to per-task log files, and
//! starts a supervisor thread that updates the record (and emits
//! an OS notification) when the child exits. The pipeline calls
//! this from E1-5's trigger-phrase path; tests call it directly
//! against fixture workers.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use tracing::{debug, error, info};

use super::record::{Task, TaskStatus};
use crate::workers::{WorkerHandle, WorkerInvocation};

/// Maximum number of characters from stdout to pull into the
/// task's `summary` field. 500 keeps OS notifications readable
/// without truncating mid-word in 99% of cases. Spec 0011's
/// "auto-summarise via LLM" is deferred to v2.
const SUMMARY_CHAR_CAP: usize = 500;

/// Spawn `worker` as a detached background task. Returns the
/// created [`Task`] record (already persisted with `status =
/// Running` and the child PID) and the join handle for the
/// supervisor thread. Production callers ignore the handle —
/// the supervisor self-manages. Tests join it to wait for the
/// child's lifecycle to complete deterministically.
///
/// Errors:
/// - The worker isn't detachable
///   (its [`WorkerHandle::detachable_argv`] returned `None`).
///   That happens for built-in handlers and for manifest workers
///   without `async_eligible = true`.
/// - The spawn fails (binary missing, exec error). The error
///   surfaces immediately; no task record is created in that
///   case.
pub fn spawn_async_task(
    worker: &dyn WorkerHandle,
    invocation: &WorkerInvocation<'_>,
    base_dir: &Path,
    thread_id: &str,
    user_intent: &str,
) -> Result<(Task, JoinHandle<()>)> {
    let argv = worker.detachable_argv(invocation).ok_or_else(|| {
        anyhow!(
            "worker {:?} is not detachable: built-in handlers and \
             manifests without async_eligible=true cannot be spawned \
             as background tasks",
            worker.id()
        )
    })?;
    if argv.is_empty() {
        return Err(anyhow!(
            "worker {:?} returned an empty detachable_argv",
            worker.id()
        ));
    }

    // Create the task record (status=Running, no pid yet). Save
    // happens after we know the pid.
    let mut task = Task::new(worker.id(), argv.clone(), user_intent, thread_id);
    let task_dir = task.dir(base_dir);
    fs::create_dir_all(&task_dir)
        .with_context(|| format!("creating task dir {}", task_dir.display()))?;

    // Open the log files. Stdio::from() takes ownership; the
    // child inherits the fds and we don't have to keep them on
    // our side. Using std::fs::File and not OpenOptions because
    // the directory was just created and the file shouldn't
    // exist yet.
    let stdout_file = File::create(task.stdout_path(base_dir))
        .with_context(|| format!("creating stdout file for task {}", task.id))?;
    let stderr_file = File::create(task.stderr_path(base_dir))
        .with_context(|| format!("creating stderr file for task {}", task.id))?;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(stdout_file));
    cmd.stderr(Stdio::from(stderr_file));
    // Same env contract the synchronous invoke path uses —
    // worker-side Stop hooks observe this to skip themselves.
    cmd.env("JARVIS_VOICE_TURN", "1");
    if let Some(cwd) = invocation.cwd {
        cmd.current_dir(cwd);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawning async worker {:?}", worker.id()))?;
    let pid = child.id();
    task.pid = Some(pid);
    task.save(base_dir)
        .with_context(|| format!("persisting initial record for task {}", task.id))?;

    info!(
        task_id = %task.id,
        worker = %task.worker_id,
        pid,
        "async task spawned"
    );

    let task_clone = task.clone();
    let base_clone = base_dir.to_path_buf();
    let supervisor = thread::Builder::new()
        .name(format!("jarvis-task-{}", task.id))
        .spawn(move || {
            supervise(child, task_clone, base_clone);
        })
        .with_context(|| format!("spawning supervisor for task {}", task.id))?;

    Ok((task, supervisor))
}

/// Supervisor body: wait for the child, update the record, emit
/// notification. Runs in its own thread, owns its `Child`. Any
/// failure becomes a logged warning + a best-effort record
/// update — we never panic the daemon over a child's exit
/// status.
fn supervise(mut child: Child, mut task: Task, base_dir: PathBuf) {
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            error!(
                task_id = %task.id,
                error = %e,
                "supervisor failed to wait on child"
            );
            task.status = TaskStatus::Failed;
            task.completion_time = Some(unix_now());
            task.pid = None;
            task.summary = Some(format!("supervisor wait failed: {e}"));
            let _ = task.save(&base_dir);
            return;
        }
    };

    task.completion_time = Some(unix_now());
    task.pid = None;
    task.exit_code = status.code();
    task.status = if status.success() {
        TaskStatus::Completed
    } else {
        // The cancel path (E1-4) sets status=Cancelled BEFORE
        // sending SIGTERM and saves; the supervisor then sees
        // a non-success status but mustn't overwrite Cancelled.
        // Re-read the on-disk record to check.
        if let Ok(disk_task) = Task::load(&base_dir, &task.id)
            && disk_task.status == TaskStatus::Cancelled
        {
            TaskStatus::Cancelled
        } else {
            TaskStatus::Failed
        }
    };
    task.summary = summarise_stdout(&task.stdout_path(&base_dir));

    if let Err(e) = task.save(&base_dir) {
        error!(
            task_id = %task.id,
            error = %e,
            "failed to persist final task record"
        );
    } else {
        info!(
            task_id = %task.id,
            status = ?task.status,
            exit_code = ?task.exit_code,
            "async task finished"
        );
    }

    notify_completion(&task);
}

fn summarise_stdout(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prefix: String = trimmed.chars().take(SUMMARY_CHAR_CAP).collect();
    if trimmed.chars().count() > SUMMARY_CHAR_CAP {
        Some(format!("{prefix}…"))
    } else {
        Some(prefix)
    }
}

fn notify_completion(task: &Task) {
    use notify_rust::Notification;
    let icon = match task.status {
        TaskStatus::Completed => "✓",
        TaskStatus::Failed => "✗",
        TaskStatus::Cancelled => "⊘",
        TaskStatus::Orphaned => "?",
        TaskStatus::Running => "…", // shouldn't happen at this call
    };
    let title = match task.status {
        TaskStatus::Completed => format!("{icon} {} completed", task.worker_id),
        TaskStatus::Failed => format!(
            "{icon} {} failed (exit {})",
            task.worker_id,
            task.exit_code.unwrap_or(-1)
        ),
        TaskStatus::Cancelled => format!("{icon} {} cancelled", task.worker_id),
        _ => format!("{icon} {} {:?}", task.worker_id, task.status),
    };
    let body = task
        .summary
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("(no output captured)");
    if let Err(e) = Notification::new()
        .summary(&title)
        .body(body)
        .appname("Jarvis")
        .show()
    {
        debug!(
            task_id = %task.id,
            error = %e,
            "OS notification failed (no notification daemon?) — continuing"
        );
    }
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
    use crate::workers::ManifestWorker;
    use crate::workers::manifest::WorkerManifest;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn async_manifest(id: &str, sh_script: &str) -> WorkerManifest {
        WorkerManifest {
            id: id.to_string(),
            description: None,
            command: vec!["sh".to_string(), "-c".to_string(), sh_script.to_string()],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: true,
            tty: false,
            dispatch_hint: None,
        }
    }

    fn sync_manifest(id: &str) -> WorkerManifest {
        WorkerManifest {
            id: id.to_string(),
            description: None,
            command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        }
    }

    /// A worker without `async_eligible = true` refuses spawn.
    /// The error names the worker so the caller knows what to
    /// fix.
    #[test]
    fn rejects_workers_without_async_eligible() {
        let tmp = TempDir::new().unwrap();
        let worker =
            ManifestWorker::new(sync_manifest("sync"), PathBuf::from("test.toml")).unwrap();
        let result = spawn_async_task(
            &worker,
            &WorkerInvocation {
                prompt: "ignored",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "test intent",
        );
        let err = result.expect_err("should reject sync worker");
        let msg = format!("{err:#}");
        assert!(msg.contains("sync"), "got: {msg}");
        assert!(msg.contains("not detachable"), "got: {msg}");
    }

    /// Happy path: async-eligible worker spawns, supervisor
    /// waits, record transitions to Completed with the captured
    /// stdout as summary.
    #[test]
    fn spawns_async_worker_and_records_completion() {
        let tmp = TempDir::new().unwrap();
        let worker = ManifestWorker::new(
            async_manifest("echo-task", "printf 'task output here'; exit 0"),
            PathBuf::from("test.toml"),
        )
        .unwrap();
        let (task, supervisor) = spawn_async_task(
            &worker,
            &WorkerInvocation {
                prompt: "ignored",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "spit out a phrase",
        )
        .expect("spawn succeeds");

        // Block until the supervisor finishes its update cycle.
        supervisor.join().expect("supervisor thread joined");

        // Reload the record from disk and inspect it.
        let final_record = Task::load(tmp.path(), &task.id).expect("record readable");
        assert_eq!(final_record.status, TaskStatus::Completed);
        assert_eq!(final_record.exit_code, Some(0));
        assert!(final_record.pid.is_none(), "pid cleared on terminal state");
        assert!(final_record.completion_time.is_some());
        assert_eq!(final_record.summary.as_deref(), Some("task output here"));
        // stdout file has the same bytes.
        let stdout = fs::read_to_string(final_record.stdout_path(tmp.path())).unwrap();
        assert_eq!(stdout, "task output here");
    }

    /// A non-zero exit transitions to `Failed` and keeps the
    /// exit code on the record. Stderr lands in the sibling
    /// file even if stdout is empty.
    #[test]
    fn nonzero_exit_marks_failed() {
        let tmp = TempDir::new().unwrap();
        let worker = ManifestWorker::new(
            async_manifest("boom-task", "echo problem >&2; exit 7"),
            PathBuf::from("test.toml"),
        )
        .unwrap();
        let (task, supervisor) = spawn_async_task(
            &worker,
            &WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "trigger an error",
        )
        .unwrap();
        supervisor.join().unwrap();

        let final_record = Task::load(tmp.path(), &task.id).unwrap();
        assert_eq!(final_record.status, TaskStatus::Failed);
        assert_eq!(final_record.exit_code, Some(7));
        let stderr = fs::read_to_string(final_record.stderr_path(tmp.path())).unwrap();
        assert!(stderr.contains("problem"), "stderr captured: {stderr}");
    }

    /// Long stdout gets truncated in the summary at the
    /// 500-char cap with an ellipsis, but the full content
    /// stays on disk for `jarvis task show`.
    #[test]
    fn long_output_summary_is_capped_with_ellipsis() {
        let tmp = TempDir::new().unwrap();
        let worker = ManifestWorker::new(
            async_manifest("big", "yes a | head -n 100"), // 200 chars by itself; we want >500
            PathBuf::from("test.toml"),
        )
        .unwrap();
        // Use a bigger script: 1000 "x" chars.
        let big_worker = ManifestWorker::new(
            async_manifest("bigger", "printf 'x%.0s' $(seq 1 1000)"),
            PathBuf::from("test.toml"),
        )
        .unwrap();
        let _ = worker; // silence unused; bigger is the one we test
        let (task, supervisor) = spawn_async_task(
            &big_worker,
            &WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "produce a wall of text",
        )
        .unwrap();
        supervisor.join().unwrap();

        let final_record = Task::load(tmp.path(), &task.id).unwrap();
        let summary = final_record.summary.clone().expect("summary present");
        // 500 chars + the ellipsis character.
        let summary_chars: usize = summary.chars().count();
        assert!(
            summary_chars > SUMMARY_CHAR_CAP && summary_chars <= SUMMARY_CHAR_CAP + 1,
            "summary should be ~{} chars, got {}",
            SUMMARY_CHAR_CAP + 1,
            summary_chars
        );
        assert!(summary.ends_with('…'), "expected ellipsis suffix");

        let full = fs::read_to_string(final_record.stdout_path(tmp.path())).unwrap();
        assert_eq!(full.chars().count(), 1000);
    }

    /// Empty stdout → `None` summary. The notification body
    /// will fall back to "(no output captured)" in that case.
    #[test]
    fn empty_output_yields_none_summary() {
        let tmp = TempDir::new().unwrap();
        let worker = ManifestWorker::new(
            async_manifest("silent", "exit 0"),
            PathBuf::from("test.toml"),
        )
        .unwrap();
        let (task, supervisor) = spawn_async_task(
            &worker,
            &WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "silent task",
        )
        .unwrap();
        supervisor.join().unwrap();

        let final_record = Task::load(tmp.path(), &task.id).unwrap();
        assert_eq!(final_record.status, TaskStatus::Completed);
        assert!(final_record.summary.is_none());
    }

    /// The initial save (with PID, status=Running) is written
    /// BEFORE the supervisor thread launches, so the registry
    /// at base_dir can see the task while it runs.
    #[test]
    fn initial_record_visible_during_run() {
        let tmp = TempDir::new().unwrap();
        // Sleep briefly so we can observe the running state
        // before the supervisor finishes.
        let worker = ManifestWorker::new(
            async_manifest("slow", "sleep 0.1; printf done"),
            PathBuf::from("test.toml"),
        )
        .unwrap();
        let (task, supervisor) = spawn_async_task(
            &worker,
            &WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            },
            tmp.path(),
            "test-thread",
            "slow task",
        )
        .unwrap();

        // Read the record before joining the supervisor.
        let mid_record = Task::load(tmp.path(), &task.id).expect("record exists");
        assert_eq!(mid_record.status, TaskStatus::Running);
        assert!(mid_record.pid.is_some());
        assert_eq!(mid_record.user_intent, "slow task");

        supervisor.join().unwrap();
        let final_record = Task::load(tmp.path(), &task.id).unwrap();
        assert_eq!(final_record.status, TaskStatus::Completed);
    }

    /// Suppress the unused-import for `Arc` if we don't actually
    /// use it in tests (we don't, but ManifestWorker uses it
    /// internally).
    #[allow(dead_code)]
    fn _silence_unused() -> Arc<()> {
        Arc::new(())
    }
}
