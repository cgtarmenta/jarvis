//! Shared cancellation primitive — sets `status = Cancelled`
//! on the on-disk record *before* sending SIGTERM, so the
//! supervisor thread (E1-3) honours the user's intent when it
//! observes the resulting non-zero exit.
//!
//! Used by `cli::cmd_task_cancel` (the CLI surface, spec 0011)
//! and `handlers::task_cancel::TaskCancelHandler` (the voice
//! surface, spec 0012). Centralising the logic keeps the two
//! paths from drifting on the cancel/Failed disambiguation
//! contract.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

use super::record::{Task, TaskStatus};

/// Cancel a running task. The flow is:
///
/// 1. Reject non-running tasks with a clear error message
///    (the CLI / voice handler relays this verbatim).
/// 2. Set `status = Cancelled` on disk so the supervisor
///    doesn't downgrade it to `Failed` when the SIGTERM
///    forces a non-zero exit.
/// 3. Send SIGTERM to the recorded PID.
///
/// Returns the updated `Task` record (with the new status).
/// SIGKILL escalation after a grace period is a v2 improvement;
/// real-world async-eligible workers (claude, gemini) honour
/// SIGTERM cleanly, so v1 trusts the signal.
pub fn cancel_task(task: &Task, base_dir: &Path) -> Result<Task> {
    if task.status != TaskStatus::Running {
        return Err(anyhow!(
            "task {} is not running (status = {:?})",
            task.id,
            task.status
        ));
    }
    let pid = task
        .pid
        .ok_or_else(|| anyhow!("task {} is Running but has no recorded pid", task.id))?;

    let mut updated = task.clone();
    updated.status = TaskStatus::Cancelled;
    updated
        .save(base_dir)
        .with_context(|| format!("persisting Cancelled state for task {}", updated.id))?;

    send_sigterm(pid).with_context(|| format!("signalling task {} (pid {pid})", updated.id))?;
    Ok(updated)
}

/// Best-effort SIGTERM to `pid`. Returns `Err` if the call
/// fails for any reason other than "process already gone".
#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<()> {
    // SAFETY: libc::kill with a real signal is well-defined for
    // any pid_t value; we don't dereference any borrowed data.
    let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if r == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // ESRCH means the process already exited — that's fine, the
    // supervisor will pick up the exit. Anything else is a real
    // failure to report.
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(anyhow::Error::new(err))
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) -> Result<()> {
    Err(anyhow!("SIGTERM not supported on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn running_task(id: &str, pid: u32) -> Task {
        Task {
            id: id.to_string(),
            thread_id: "test".to_string(),
            worker_id: "gemini".to_string(),
            spawn_time: 0,
            completion_time: None,
            status: TaskStatus::Running,
            user_intent: "test".to_string(),
            command: vec!["true".to_string()],
            pid: Some(pid),
            exit_code: None,
            summary: None,
        }
    }

    /// `cancel_task` against a Running task with our own PID
    /// would actually kill the test process — instead we use a
    /// stale-but-not-stupidly-high PID. ESRCH gets swallowed so
    /// the call returns Ok even when the PID is gone.
    #[test]
    fn cancel_running_task_marks_and_signals() {
        let tmp = TempDir::new().unwrap();
        // Use a PID that's *almost certainly* not in use. ESRCH
        // is treated as "already gone, that's fine".
        let stale_pid: u32 = 0xFFFF_FFFE;
        let t = running_task("t-001", stale_pid);
        t.save(tmp.path()).unwrap();

        let updated = cancel_task(&t, tmp.path()).expect("cancel succeeds");
        assert_eq!(updated.status, TaskStatus::Cancelled);

        // Reload from disk — the Cancelled state is durable.
        let reloaded = Task::load(tmp.path(), &t.id).unwrap();
        assert_eq!(reloaded.status, TaskStatus::Cancelled);
    }

    /// Non-Running tasks refuse cancellation with a clear
    /// message that the CLI / voice handler can relay.
    #[test]
    fn cancel_non_running_errors_clearly() {
        let tmp = TempDir::new().unwrap();
        let mut t = running_task("t-002", 0xFFFF_FFFE);
        t.status = TaskStatus::Completed;
        t.pid = None;
        t.save(tmp.path()).unwrap();

        let err = cancel_task(&t, tmp.path()).expect_err("should refuse");
        let msg = format!("{err:#}");
        assert!(msg.contains("not running"), "got: {msg}");
        assert!(msg.contains("Completed"), "got: {msg}");
    }

    /// Running task without a recorded PID is a bug, surfaced
    /// as a precise error rather than a panic.
    #[test]
    fn cancel_running_without_pid_errors() {
        let tmp = TempDir::new().unwrap();
        let mut t = running_task("t-003", 0);
        t.pid = None;
        t.save(tmp.path()).unwrap();

        let err = cancel_task(&t, tmp.path()).expect_err("should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("no recorded pid"), "got: {msg}");
    }
}
