//! Subprocess-driven classifier backed by Warp's `oz` CLI — spec
//! 0013 / B-3.
//!
//! Spawns `oz agent run --model <model_id> --prompt <classifier
//! prompt>`, reads stdout, and parses out the chosen worker id with
//! the same [`super::parse_worker_id`] helper the HTTP backend uses.
//! Stderr is captured and used to make error messages useful when
//! the subprocess exits non-zero.
//!
//! Wire contract this backend assumes (mirrors `oz`'s documented
//! CLI surface):
//!
//! - Binary is named `oz` by default, resolvable on PATH; users with
//!   custom installs override via [`OzCliBackend::with_binary`].
//! - Argv shape: `oz agent run --model <model> --prompt <prompt>`.
//!   The prompt comes through as a single argv element (no shell
//!   interpolation, so newlines / quotes inside don't matter).
//! - Stdout contains the model's reply. We extract the *first*
//!   whitespace-delimited token as the worker id; chatty replies
//!   that lead with the id then add commentary still work.
//! - Non-zero exit code → backend error. Includes a stderr snippet
//!   so `jarvis dispatcher status` (future) and tracing fields show
//!   *why* the call failed.
//!
//! Timeout handling mirrors `recorder.rs`'s pattern: a watchdog
//! thread sends `SIGTERM` after the configured deadline. We don't
//! pull in `wait-timeout` or similar — the recorder already proved
//! the libc-based approach works cleanly on the unix targets Jarvis
//! supports.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use super::{LlmBackend, WorkerInfo, default_classifier_prompt, parse_worker_id};

/// Default per-call timeout. Same 5s the HTTP backend uses; voice-
/// turn latency budget is shared between both.
const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// `oz` subprocess classifier.
///
/// Construct with [`OzCliBackend::new`] (defaults to `oz` on PATH);
/// override the binary / timeout via the chainable setters before
/// installing it as the cascade's stage-2 backend.
pub struct OzCliBackend {
    binary: String,
    model: String,
    timeout: Duration,
}

impl OzCliBackend {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            binary: "oz".to_string(),
            model: model.into(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl LlmBackend for OzCliBackend {
    fn name(&self) -> &str {
        "oz"
    }

    fn classify(&self, prompt: &str, workers: &[WorkerInfo]) -> Result<Option<String>> {
        let classifier_prompt = default_classifier_prompt(prompt, workers);

        // Spawn with stdout + stderr piped so we can read both; stdin
        // is null because the prompt rides in argv. Closing stdin is
        // the same convention `ManifestWorker` uses when `{prompt}`
        // is in the argv template.
        //
        // On unix, also put the child in its own process group via
        // `process_group(0)`. This matters for the timeout path: the
        // real `oz` CLI is a wrapper that spawns model-runner
        // children, and SIGTERM-ing only the parent leaves those
        // children alive holding the stdout pipe open — our
        // `read_to_end` then blocks until they exit on their own.
        // Killing the whole group via `kill(-pgid, SIGTERM)` brings
        // everything down at once. Test fixtures exhibit the same
        // shape (`sh -c 'sleep 60'`), so this is also what makes
        // the timeout test deterministic.
        let mut cmd = Command::new(&self.binary);
        cmd.args([
            "agent",
            "run",
            "--model",
            &self.model,
            "--prompt",
            &classifier_prompt,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {} (classifier call)", self.binary))?;

        // Watchdog thread: SIGTERM the child if it outlives the
        // timeout. Mirrors the pattern in `recorder.rs` rather than
        // pulling in another crate. `finished` is the cross-thread
        // signal that the wait completed before the timer fired.
        let pid = child.id();
        let timeout = self.timeout;
        let finished = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let finished_for_timer = Arc::clone(&finished);
        let timed_out_for_timer = Arc::clone(&timed_out);
        let watchdog = thread::spawn(move || {
            let step = Duration::from_millis(50);
            let start = Instant::now();
            while start.elapsed() < timeout {
                if finished_for_timer.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(step);
            }
            // Mark *before* signalling so a racing reader can tell
            // "we killed it" from "it died on its own".
            timed_out_for_timer.store(true, Ordering::Relaxed);
            #[cfg(unix)]
            unsafe {
                // Negative pid → send signal to the process group
                // we set up with `process_group(0)` above.
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        });

        // Drain stdout + stderr to completion. `wait_with_output`
        // would also work but consumes the child; we want the child
        // around for the watchdog's `kill` semantics to be
        // unambiguous, and the per-stream reads keep the code symmetric
        // with the pipes branch of `ManifestWorker::invoke_pipes`.
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_end(&mut stdout_buf);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_end(&mut stderr_buf);
        }
        let status = child
            .wait()
            .with_context(|| format!("waiting on {} (classifier call)", self.binary))?;
        finished.store(true, Ordering::Relaxed);
        let _ = watchdog.join();

        if timed_out.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "{} classifier timed out after {:?}",
                self.binary,
                timeout
            ));
        }
        if !status.success() {
            let stderr_snippet = String::from_utf8_lossy(&stderr_buf);
            return Err(anyhow!(
                "{} classifier exited with {}: {}",
                self.binary,
                status,
                stderr_snippet.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&stdout_buf);
        Ok(parse_worker_id(&stdout))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// Write a `#!/bin/sh` script with mode 0755 inside `dir` and
    /// return its absolute path. Used to stand in for the real `oz`
    /// binary in tests — the backend doesn't care that "oz" is
    /// actually a fixture script as long as the argv shape is the
    /// one it expects.
    ///
    /// The retry loop below works around a transient Linux race
    /// (`ETXTBSY` / "text file busy") that surfaces under
    /// `cargo test`'s parallel runner: a sibling test's fork can
    /// transiently inherit the write fd we just closed on this
    /// script, and the kernel refuses the `exec` until that
    /// inherited fd goes away. Up to ~250ms of retries is plenty
    /// in practice — sibling forks are short-lived.
    fn fixture(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let script = format!("#!/bin/sh\n{body}\n");
        let mut attempt = 0;
        loop {
            match fs::write(&p, script.as_bytes()) {
                Ok(()) => break,
                Err(_) if attempt < 5 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("write fixture script: {e}"),
            }
        }
        let mut perms = fs::metadata(&p).expect("stat fixture").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).expect("chmod fixture");

        // Give the kernel a moment to drop any inherited fds from
        // racing sibling forks before we hand the path off to a
        // `Command::spawn`. Faster than a retry-on-spawn loop and
        // local to the test fixture (production `oz` is never
        // written concurrently).
        std::thread::sleep(Duration::from_millis(20));
        p
    }

    fn worker(id: &str, hint: Option<&str>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            dispatch_hint: hint.map(|s| s.to_string()),
        }
    }

    /// Happy path: the fixture echoes a worker id on stdout; backend
    /// extracts and returns it.
    #[test]
    fn classify_extracts_worker_id_from_stdout() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(tmp.path(), "oz-mock", "echo time");

        let backend = OzCliBackend::new("test-model").with_binary(bin.to_string_lossy());
        let result = backend
            .classify("qué hora es", &[worker("time", Some("Clock queries."))])
            .unwrap();
        assert_eq!(result.as_deref(), Some("time"));
    }

    /// Fixture echoes `none` → maps to `Ok(None)` so the cascade
    /// falls through to stage 3.
    #[test]
    fn classify_decline_yields_none() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(tmp.path(), "oz-mock", "echo none");

        let backend = OzCliBackend::new("test-model").with_binary(bin.to_string_lossy());
        let result = backend
            .classify("anything", &[worker("time", None)])
            .unwrap();
        assert!(result.is_none());
    }

    /// Chatty replies that put the id first plus commentary after
    /// still resolve. Real models do this even when told not to.
    #[test]
    fn classify_tolerates_chatty_reply() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "oz-mock",
            "echo 'task-list  -- that is the best match'",
        );

        let backend = OzCliBackend::new("test-model").with_binary(bin.to_string_lossy());
        let result = backend
            .classify("qué tareas tengo", &[worker("task-list", None)])
            .unwrap();
        assert_eq!(result.as_deref(), Some("task-list"));
    }

    /// Non-zero exit → backend error mentioning the binary name and
    /// the stderr snippet (so logs are useful).
    #[test]
    fn classify_propagates_nonzero_exit_as_error() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "oz-mock",
            "echo 'oz: model not found' >&2\nexit 7",
        );

        let backend = OzCliBackend::new("test-model").with_binary(bin.to_string_lossy());
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("nonzero exit should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("classifier exited"), "got: {msg}");
        assert!(msg.contains("model not found"), "got: {msg}");
    }

    /// Missing binary → spawn fails with a useful error.
    #[test]
    fn classify_errors_when_binary_missing() {
        let backend = OzCliBackend::new("test-model")
            .with_binary("/nonexistent/path/to/oz-binary-fixture-zzz");
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("missing binary should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("spawning"), "got: {msg}");
    }

    /// Argv passes prompt as a single element so newlines / quotes
    /// inside the classifier prompt don't break the call. Fixture
    /// here echoes argv[6] (the prompt) so we can assert it
    /// round-trips intact.
    #[test]
    fn classify_passes_prompt_as_argv_intact() {
        let tmp = TempDir::new().unwrap();
        // argv layout: $0 (script) agent run --model M --prompt P
        // → script sees argv[1..6] = [agent, run, --model, M,
        //   --prompt, P]; we want $6 = P. Print "time" first so the
        //   id parses, then a delimiter, then $6 so the test can
        //   inspect it.
        let bin = fixture(
            tmp.path(),
            "oz-mock",
            "echo time\necho ==DELIM==\nprintf '%s' \"$6\"",
        );

        let backend = OzCliBackend::new("M").with_binary(bin.to_string_lossy());
        let result = backend
            .classify(
                "hola\ncon\nsaltos \"y comillas\"",
                &[worker("time", Some("Clock queries."))],
            )
            .unwrap();
        assert_eq!(result.as_deref(), Some("time"));

        // Re-run with a fixture that echoes argv[6] (the prompt)
        // to stderr and exits non-zero. The round-tripped prompt
        // shows up in the backend's error message, which is the
        // only externally-observable surface for the *body* of
        // what got passed in argv.
        let bin2 = fixture(tmp.path(), "oz-fail-echo", "printf '%s' \"$6\" >&2\nexit 1");
        let backend = OzCliBackend::new("M").with_binary(bin2.to_string_lossy());
        let err = backend
            .classify(
                "hola\ncon\nsaltos \"y comillas\"",
                &[worker("time", Some("Clock queries."))],
            )
            .expect_err("forced fail");
        let msg = format!("{err:#}");
        // Multi-line argv element survives the round-trip intact:
        // newlines + quotes appear in the captured stderr.
        assert!(msg.contains("hola"), "prompt should round-trip: {msg}");
        assert!(msg.contains("saltos"), "prompt should round-trip: {msg}");
        assert!(msg.contains("comillas"), "prompt should round-trip: {msg}");
        // The classifier prompt includes the worker list + hint, so
        // the round-tripped argv also carries them.
        assert!(
            msg.contains("Clock queries."),
            "worker hint should round-trip: {msg}"
        );
    }

    /// Configured timeout actually applies: a fixture that sleeps
    /// far longer than the timeout gets SIGTERM'd and the backend
    /// returns a timeout-shaped error within ~timeout + a small
    /// margin.
    #[test]
    fn classify_honors_configured_timeout() {
        let tmp = TempDir::new().unwrap();
        // sleep 60 is well beyond any sensible test runtime; the
        // watchdog should SIGTERM long before.
        let bin = fixture(tmp.path(), "oz-slow", "sleep 60\necho time");

        let backend = OzCliBackend::new("M")
            .with_binary(bin.to_string_lossy())
            .with_timeout(Duration::from_millis(200));
        let start = Instant::now();
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("should time out");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "expected fast SIGTERM, took {elapsed:?}"
        );
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("timed out"),
            "expected timeout-shaped error, got: {msg}"
        );
    }

    /// `name()` returns the stable identifier used by tracing log
    /// fields. The cascade adapter (B-4) tags each turn with this
    /// value.
    #[test]
    fn name_is_stable() {
        let backend = OzCliBackend::new("M");
        assert_eq!(backend.name(), "oz");
    }
}
