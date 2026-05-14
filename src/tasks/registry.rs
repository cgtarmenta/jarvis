//! `TaskRegistry` — in-memory view of every async task on disk.
//!
//! Built at daemon startup by scanning
//! `~/.cache/jarvis/tasks/`. Records marked `Running` whose PID
//! is no longer alive (daemon crashed mid-task) get transitioned
//! to `Orphaned` and persisted with the completion timestamp.
//! Read accessors back the CLI (`jarvis task list / show /
//! cancel / clean`) and the dispatcher's "did this thread already
//! spawn the same kind of task?" lookups.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use tracing::warn;

use super::record::{Task, TaskStatus};
use crate::config;

#[derive(Debug)]
pub struct TaskRegistry {
    base_dir: PathBuf,
    tasks: Vec<Task>,
}

impl TaskRegistry {
    /// Default location: `<cache>/jarvis/tasks/`. Created if it
    /// doesn't exist yet so a fresh daemon doesn't trip on the
    /// missing path.
    pub fn default_dir() -> Result<PathBuf> {
        let dir = config::cache_dir()?.join("tasks");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Scan `dir`, parse every `record.json` found, and return
    /// the resulting registry. Never errors — per-record
    /// problems become warnings and the affected record is
    /// skipped.
    ///
    /// **Passive load**: this does NOT touch the orphan check.
    /// Daemon startup callers want
    /// [`reconcile_orphans`](Self::reconcile_orphans) after
    /// loading; CLI callers (`jarvis task list/show`) skip it
    /// because they shouldn't transition state in a
    /// query-only operation.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut tasks = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    base_dir: dir.to_path_buf(),
                    tasks,
                };
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "reading tasks dir");
                return Self {
                    base_dir: dir.to_path_buf(),
                    tasks,
                };
            }
        };

        // Sort by entry name so the registry order is reproducible
        // and matches the `t-<unix>-<hex>` sort-by-spawn-time
        // semantic that `task_id` was designed for.
        let mut task_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        task_dirs.sort();

        for task_dir in task_dirs {
            let Some(id) = task_dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            match Task::load(dir, id) {
                Ok(task) => tasks.push(task),
                Err(e) => {
                    warn!(
                        task_dir = %task_dir.display(),
                        error = %format!("{e:#}"),
                        "skipping unreadable task record"
                    );
                }
            }
        }

        Self {
            base_dir: dir.to_path_buf(),
            tasks,
        }
    }

    /// Walk every `Running` record and transition it to
    /// `Orphaned`. The daemon calls this once at startup: any
    /// task left in `Running` state on disk implies the previous
    /// daemon process died before its watcher thread could
    /// update the record. We can't re-adopt the child (it's
    /// reparented to init and not waitable from us anymore), so
    /// we mark it orphaned and surface what we know in the
    /// summary.
    ///
    /// Mid-flight callers (CLI, intra-process queries) must NOT
    /// run this — they'd false-orphan tasks the current daemon
    /// is supervising in memory.
    pub fn reconcile_orphans(&mut self) {
        let base = self.base_dir.clone();
        for task in self.tasks.iter_mut() {
            if task.status != TaskStatus::Running {
                continue;
            }
            let alive = task.pid.map(pid_alive).unwrap_or(false);
            if alive {
                warn!(
                    task_id = %task.id,
                    pid = ?task.pid,
                    "running task survived daemon restart; can't wait on it"
                );
            }
            let was_pid = task.pid;
            task.status = TaskStatus::Orphaned;
            task.completion_time = Some(unix_now());
            task.pid = None;
            let summary = match (alive, was_pid) {
                (true, Some(p)) => format!(
                    "Daemon restarted while task was running. Child PID {p} is still alive but no longer trackable; check with `ps` or kill manually."
                ),
                _ => "Daemon restarted while task was running; the child process is no longer reachable.".to_string(),
            };
            task.summary = Some(summary);
            if let Err(e) = task.save(&base) {
                warn!(
                    task_id = %task.id,
                    error = %format!("{e:#}"),
                    "failed to persist orphan transition"
                );
            }
        }
    }

    pub fn all(&self) -> &[Task] {
        &self.tasks
    }

    pub fn active(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| !t.status.is_terminal())
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Find one task by id-prefix. Returns:
    ///
    /// * `Ok(Some(task))` — exactly one match.
    /// * `Ok(None)` — zero matches.
    /// * `Err(...)` — two or more matches; user prefix is too
    ///   short. The error message lists the conflicting ids so
    ///   the user knows what to type next.
    pub fn find_by_prefix(&self, prefix: &str) -> Result<Option<&Task>> {
        let matches: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.id.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0])),
            _ => Err(anyhow!(
                "prefix {prefix:?} is ambiguous ({} matches: {})",
                matches.len(),
                matches
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// Cheap "does this PID exist on this host?" check using
/// `kill(pid, 0)`. Returns `true` if the process exists (even if
/// we don't have permission to signal it — `EPERM` still means
/// the process is real). Returns `false` for `ESRCH` (no such
/// process) and any other failure.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: libc::kill is safe to call with signal 0; it only
    // probes for existence and doesn't modify any process state.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0);
    // EPERM means the process exists but we can't signal it —
    // still alive from our POV.
    errno == libc::EPERM
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Non-unix targets fall through to "assume dead" so the
    // orphan check is conservative there. Voice-loop targets
    // are all unix today.
    false
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
    use tempfile::TempDir;

    fn record_with_status(id: &str, status: TaskStatus, pid: Option<u32>) -> Task {
        Task {
            id: id.to_string(),
            thread_id: "test-thread".to_string(),
            worker_id: "gemini".to_string(),
            spawn_time: 1_715_700_000,
            completion_time: None,
            status,
            user_intent: "test intent".to_string(),
            command: vec!["echo".to_string(), "hi".to_string()],
            pid,
            exit_code: None,
            summary: None,
        }
    }

    /// Missing directory → empty registry. The daemon must
    /// start successfully even when no task has ever been
    /// spawned.
    #[test]
    fn missing_dir_yields_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let reg = TaskRegistry::load_from_dir(&tmp.path().join("nonexistent"));
        assert!(reg.all().is_empty());
    }

    /// Terminal-state records load unchanged. The orphan check
    /// is a no-op for anything that isn't `Running`.
    #[test]
    fn terminal_records_load_unchanged() {
        let tmp = TempDir::new().unwrap();
        let mut t = record_with_status("t-001", TaskStatus::Completed, None);
        t.exit_code = Some(0);
        t.save(tmp.path()).unwrap();

        let reg = TaskRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.all().len(), 1);
        let loaded = reg.get("t-001").unwrap();
        assert_eq!(loaded.status, TaskStatus::Completed);
        assert_eq!(loaded.exit_code, Some(0));
    }

    /// `reconcile_orphans` transitions every `Running` record
    /// to `Orphaned` and persists the change. The change is
    /// durable: a second load from the same dir reads back the
    /// new state. Dead-PID case.
    #[test]
    fn reconcile_orphans_dead_pid_marks_and_persists() {
        let tmp = TempDir::new().unwrap();
        let stale_pid: u32 = 0xFFFF_FFFE;
        let t = record_with_status("t-stale", TaskStatus::Running, Some(stale_pid));
        t.save(tmp.path()).unwrap();

        let mut reg = TaskRegistry::load_from_dir(tmp.path());
        reg.reconcile_orphans();

        let loaded = reg.get("t-stale").unwrap();
        assert_eq!(loaded.status, TaskStatus::Orphaned);
        assert!(loaded.pid.is_none());
        assert!(loaded.completion_time.is_some());
        assert!(
            loaded
                .summary
                .as_deref()
                .unwrap_or("")
                .contains("no longer reachable"),
            "summary for dead-PID case: {:?}",
            loaded.summary
        );

        // Persistence: reload sees the new state.
        let reg2 = TaskRegistry::load_from_dir(tmp.path());
        assert_eq!(reg2.get("t-stale").unwrap().status, TaskStatus::Orphaned);
    }

    /// Alive-PID case: orphan check still transitions to
    /// `Orphaned` because we can't re-adopt the child for
    /// `waitpid`, but the summary message tells the user the
    /// process is still alive and how to deal with it.
    #[test]
    fn reconcile_orphans_alive_pid_marks_with_warning_summary() {
        let tmp = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let t = record_with_status("t-alive", TaskStatus::Running, Some(my_pid));
        t.save(tmp.path()).unwrap();

        let mut reg = TaskRegistry::load_from_dir(tmp.path());
        reg.reconcile_orphans();

        let loaded = reg.get("t-alive").unwrap();
        assert_eq!(loaded.status, TaskStatus::Orphaned);
        assert!(
            loaded
                .summary
                .as_deref()
                .unwrap_or("")
                .contains("still alive"),
            "summary for alive-PID case should mention `still alive`: {:?}",
            loaded.summary
        );
    }

    /// Passive load (no `reconcile_orphans`) leaves `Running`
    /// records untouched. Mid-flight CLI queries and
    /// in-process registry refreshes rely on this — the daemon
    /// supervising the task in memory should not have its
    /// tracking invalidated by a registry reload.
    #[test]
    fn passive_load_leaves_running_intact() {
        let tmp = TempDir::new().unwrap();
        let my_pid = std::process::id();
        record_with_status("t-supervised", TaskStatus::Running, Some(my_pid))
            .save(tmp.path())
            .unwrap();

        let reg = TaskRegistry::load_from_dir(tmp.path());
        assert_eq!(reg.get("t-supervised").unwrap().status, TaskStatus::Running);
    }

    /// `active()` returns only `Running` tasks after a passive
    /// load — the partition is what the CLI's `--active` flag
    /// will filter on (E1-4).
    #[test]
    fn active_filters_to_running_only_after_passive_load() {
        let tmp = TempDir::new().unwrap();
        record_with_status("t-a", TaskStatus::Completed, None)
            .save(tmp.path())
            .unwrap();
        record_with_status("t-b", TaskStatus::Failed, None)
            .save(tmp.path())
            .unwrap();
        let my_pid = std::process::id();
        record_with_status("t-c", TaskStatus::Running, Some(my_pid))
            .save(tmp.path())
            .unwrap();

        let reg = TaskRegistry::load_from_dir(tmp.path());
        let active_ids: Vec<&str> = reg.active().map(|t| t.id.as_str()).collect();
        assert_eq!(active_ids, vec!["t-c"]);
    }

    /// `find_by_prefix` resolves unique prefixes and errors on
    /// ambiguity. Backs the CLI's id-prefix convenience.
    #[test]
    fn prefix_lookup_unique_versus_ambiguous() {
        let tmp = TempDir::new().unwrap();
        record_with_status("t-1715700000-aaaaaa", TaskStatus::Completed, None)
            .save(tmp.path())
            .unwrap();
        record_with_status("t-1715700000-bbbbbb", TaskStatus::Completed, None)
            .save(tmp.path())
            .unwrap();
        record_with_status("t-1715800000-cccccc", TaskStatus::Completed, None)
            .save(tmp.path())
            .unwrap();

        let reg = TaskRegistry::load_from_dir(tmp.path());
        // Unique prefix.
        let one = reg.find_by_prefix("t-1715800").unwrap();
        assert_eq!(one.unwrap().id, "t-1715800000-cccccc");
        // Zero-match prefix.
        let none = reg.find_by_prefix("t-9999").unwrap();
        assert!(none.is_none());
        // Ambiguous prefix → Err with both ids in the message.
        let err = reg.find_by_prefix("t-1715700").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ambiguous"), "got: {msg}");
        assert!(msg.contains("aaaaaa"), "got: {msg}");
        assert!(msg.contains("bbbbbb"), "got: {msg}");
    }

    /// A garbage directory entry (not a real task) is logged and
    /// skipped — the rest of the registry still loads.
    #[test]
    fn skips_unreadable_task_directories() {
        let tmp = TempDir::new().unwrap();
        // Good record.
        record_with_status("t-good", TaskStatus::Completed, None)
            .save(tmp.path())
            .unwrap();
        // Bad record: directory exists but record.json is gibberish.
        let bad = tmp.path().join("t-bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("record.json"), "this is not json").unwrap();

        let reg = TaskRegistry::load_from_dir(tmp.path());
        assert!(reg.get("t-good").is_some(), "good record still loads");
        assert!(reg.get("t-bad").is_none(), "bad record skipped");
    }

    /// `pid_alive` correctly identifies the current process as
    /// alive. Cheap smoke that the libc::kill wiring works on
    /// this platform.
    #[test]
    #[cfg(unix)]
    fn pid_alive_for_self() {
        let me = std::process::id();
        assert!(pid_alive(me), "the test process should be alive");
        // Pid 0xFFFFFFFE almost certainly doesn't exist.
        assert!(!pid_alive(0xFFFF_FFFE), "high PID should be dead");
    }
}
