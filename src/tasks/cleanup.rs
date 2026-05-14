//! Task auto-prune — keeps the on-disk task registry bounded.
//!
//! Spec 0011 / E1-5: the daemon calls
//! [`autoprune_terminal_tasks`] at startup, after the orphan
//! reconciliation pass, to drop the oldest terminal-status
//! records so the cache directory doesn't grow without bound.
//! Active (`Running`) tasks are never affected; only the
//! completed/failed/cancelled/orphaned tail gets capped.

use std::fs;
use std::path::Path;

use tracing::warn;

use super::registry::TaskRegistry;

/// Trim the on-disk task collection so at most `max_retained`
/// terminal-status records remain. Returns the number of task
/// directories removed.
///
/// FIFO eviction: the *oldest* terminal records get pruned
/// first, where "oldest" is determined by `completion_time`
/// (falling back to `spawn_time` for orphans without one).
/// Running tasks count toward neither the cap nor the eviction
/// list.
pub fn autoprune_terminal_tasks(
    base_dir: &Path,
    registry: &TaskRegistry,
    max_retained: usize,
) -> usize {
    let mut terminal: Vec<&super::record::Task> = registry
        .all()
        .iter()
        .filter(|t| t.status.is_terminal())
        .collect();

    // Sort by age — newest first. The first `max_retained`
    // stay; everything after is evicted.
    terminal.sort_by(|a, b| {
        let a_t = a.completion_time.unwrap_or(a.spawn_time);
        let b_t = b.completion_time.unwrap_or(b.spawn_time);
        b_t.cmp(&a_t)
    });

    if terminal.len() <= max_retained {
        return 0;
    }

    let mut pruned = 0;
    for task in terminal.iter().skip(max_retained) {
        let task_dir = task.dir(base_dir);
        match fs::remove_dir_all(&task_dir) {
            Ok(()) => pruned += 1,
            Err(e) => warn!(
                task_id = %task.id,
                dir = %task_dir.display(),
                error = %e,
                "autoprune: failed to remove task directory"
            ),
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::record::{Task, TaskStatus};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    fn record(id: &str, status: TaskStatus, spawn: u64, completion: Option<u64>) -> Task {
        Task {
            id: id.to_string(),
            thread_id: "test".to_string(),
            worker_id: "gemini".to_string(),
            spawn_time: spawn,
            completion_time: completion,
            status,
            user_intent: "test".to_string(),
            command: vec!["true".to_string()],
            pid: None,
            exit_code: None,
            summary: None,
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Under the cap: no records are pruned.
    #[test]
    fn under_cap_prunes_nothing() {
        let tmp = TempDir::new().unwrap();
        let t = now();
        record("t-001", TaskStatus::Completed, t, Some(t))
            .save(tmp.path())
            .unwrap();
        record("t-002", TaskStatus::Completed, t - 10, Some(t - 5))
            .save(tmp.path())
            .unwrap();
        let reg = TaskRegistry::load_from_dir(tmp.path());

        let pruned = autoprune_terminal_tasks(tmp.path(), &reg, 5);
        assert_eq!(pruned, 0);
        assert!(tmp.path().join("t-001").exists());
        assert!(tmp.path().join("t-002").exists());
    }

    /// Over the cap: oldest terminal records drop, newest stay.
    /// Eviction order is FIFO by completion time.
    #[test]
    fn over_cap_drops_oldest_first() {
        let tmp = TempDir::new().unwrap();
        let t = now();
        // Three terminal tasks at descending completion times.
        record("t-newest", TaskStatus::Completed, t, Some(t))
            .save(tmp.path())
            .unwrap();
        record("t-middle", TaskStatus::Failed, t - 100, Some(t - 50))
            .save(tmp.path())
            .unwrap();
        record("t-oldest", TaskStatus::Completed, t - 1000, Some(t - 900))
            .save(tmp.path())
            .unwrap();
        let reg = TaskRegistry::load_from_dir(tmp.path());

        let pruned = autoprune_terminal_tasks(tmp.path(), &reg, 2);
        assert_eq!(pruned, 1);
        assert!(tmp.path().join("t-newest").exists());
        assert!(tmp.path().join("t-middle").exists());
        assert!(
            !tmp.path().join("t-oldest").exists(),
            "oldest should be pruned"
        );
    }

    /// Active tasks never count against the cap and are never
    /// evicted, even when older than retained terminal records.
    #[test]
    fn active_tasks_are_never_pruned() {
        let tmp = TempDir::new().unwrap();
        let t = now();
        // One active task (older than all the terminal ones).
        let mut running = record("t-active", TaskStatus::Running, t - 10_000, None);
        running.pid = Some(std::process::id());
        running.save(tmp.path()).unwrap();
        // Three terminal tasks above the cap of 1.
        for i in 0..3 {
            record(
                &format!("t-term-{i}"),
                TaskStatus::Completed,
                t - i * 100,
                Some(t - i * 100 + 10),
            )
            .save(tmp.path())
            .unwrap();
        }
        let reg = TaskRegistry::load_from_dir(tmp.path());

        let pruned = autoprune_terminal_tasks(tmp.path(), &reg, 1);
        assert_eq!(pruned, 2, "two of the three terminal tasks removed");
        assert!(
            tmp.path().join("t-active").exists(),
            "active task survives autoprune regardless of age"
        );
    }

    /// `max_retained = 0` is permitted: every terminal record
    /// gets pruned. Useful for opportunistic cleanup.
    #[test]
    fn zero_retain_drops_all_terminal() {
        let tmp = TempDir::new().unwrap();
        let t = now();
        record("t-a", TaskStatus::Completed, t, Some(t))
            .save(tmp.path())
            .unwrap();
        record("t-b", TaskStatus::Failed, t - 50, Some(t - 40))
            .save(tmp.path())
            .unwrap();
        let reg = TaskRegistry::load_from_dir(tmp.path());

        let pruned = autoprune_terminal_tasks(tmp.path(), &reg, 0);
        assert_eq!(pruned, 2);
        assert!(!tmp.path().join("t-a").exists());
        assert!(!tmp.path().join("t-b").exists());
    }
}
