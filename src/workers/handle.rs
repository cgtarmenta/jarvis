//! `WorkerHandle` — the runtime trait shared by manifest-loaded external
//! workers and built-in Rust handlers.
//!
//! The dispatcher (hija A) and downstream callers always interact with
//! workers through this trait, so the same code path drives `time` (a
//! Rust function call), `claude` (a subprocess pipe), and any
//! `oz`/`gemini`-style external CLI the user has declared as a manifest.
//!
//! `ManifestWorker` is the implementation that wraps a `WorkerManifest`
//! plus its pre-compiled `session_id_capture` regex. Built-in handlers
//! implement the trait directly in their own modules (hija A populates
//! `src/handlers/`); this commit only sets the contract.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use regex::Regex;

use super::manifest::{SessionIdSource, WorkerManifest};

/// Inputs handed to a worker by the dispatcher at invocation time.
///
/// The dispatcher is responsible for resolving the user's utterance into
/// a complete `prompt` (including any context the worker needs but
/// doesn't carry itself). Stateful workers receive their last-known
/// `session_id` for resumption; stateless workers ignore it.
#[derive(Debug, Clone)]
pub struct WorkerInvocation<'a> {
    pub prompt: &'a str,
    pub session_id: Option<&'a str>,
    pub cwd: Option<&'a str>,
}

/// What a worker returns. `captured_session_id` is set iff the worker
/// has a `session_id_capture` rule that matched on this invocation —
/// the dispatcher writes that value into the session's `active_workers`
/// map (per spec D).
#[derive(Debug, Clone)]
pub struct WorkerResponse {
    pub text: String,
    pub captured_session_id: Option<String>,
}

/// The contract every worker (built-in or manifest-loaded) implements.
///
/// Default methods return safe defaults for the introspection surface;
/// only `id` and `invoke` are required, so built-in handlers can be
/// terse. Object-safe — used as `Arc<dyn WorkerHandle>` in the registry.
pub trait WorkerHandle: Send + Sync {
    /// Unique identifier; matches the `id` field of the manifest or the
    /// hard-coded id for built-in handlers (e.g. `"time"`, `"calc"`).
    fn id(&self) -> &str;

    fn description(&self) -> Option<&str> {
        None
    }

    fn dispatch_hint(&self) -> Option<&str> {
        None
    }

    fn stateful(&self) -> bool {
        false
    }

    fn async_eligible(&self) -> bool {
        false
    }

    fn tty(&self) -> bool {
        false
    }

    /// The manifest file this worker was loaded from, if any. Built-in
    /// handlers return `None`; external manifests return `Some(path)`.
    /// Used by `jarvis worker list` to show provenance.
    fn source_path(&self) -> Option<&Path> {
        None
    }

    /// Run the worker once and return its reply (plus any captured
    /// session id). Synchronous; async invocation lives in hija E1.
    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse>;
}

/// `WorkerHandle` impl for an externally-defined manifest worker. Wraps
/// the parsed `WorkerManifest` and (when configured) the pre-compiled
/// `session_id_capture` regex.
#[derive(Debug)]
pub struct ManifestWorker {
    manifest: WorkerManifest,
    source: PathBuf,
    capture_regex: Option<Regex>,
}

impl ManifestWorker {
    /// Build a `ManifestWorker`, compiling its capture regex if any. The
    /// registry's `load_one` returns the resulting `Result` and disables
    /// the worker on regex compile failure — keep this consistent if you
    /// add construction sites elsewhere.
    pub fn new(manifest: WorkerManifest, source: PathBuf) -> Result<Self> {
        let capture_regex = match &manifest.session_id_capture {
            Some(cap) => Some(
                Regex::new(&cap.regex).context("compiling session_id_capture.regex")?,
            ),
            None => None,
        };
        Ok(Self {
            manifest,
            source,
            capture_regex,
        })
    }

    /// Read-only access to the underlying manifest. Useful for the
    /// dispatcher (which inspects `command`, `initial_command`, etc.)
    /// and for `jarvis worker list` (introspection).
    pub fn manifest(&self) -> &WorkerManifest {
        &self.manifest
    }
}

impl WorkerHandle for ManifestWorker {
    fn id(&self) -> &str {
        &self.manifest.id
    }

    fn description(&self) -> Option<&str> {
        self.manifest.description.as_deref()
    }

    fn dispatch_hint(&self) -> Option<&str> {
        self.manifest.dispatch_hint.as_deref()
    }

    fn stateful(&self) -> bool {
        self.manifest.stateful
    }

    fn async_eligible(&self) -> bool {
        self.manifest.async_eligible
    }

    fn tty(&self) -> bool {
        self.manifest.tty
    }

    fn source_path(&self) -> Option<&Path> {
        Some(&self.source)
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let mut values: HashMap<&str, &str> = HashMap::new();
        values.insert("prompt", ctx.prompt);
        if let Some(sid) = ctx.session_id {
            values.insert("session_id", sid);
        }
        if let Some(cwd) = ctx.cwd {
            values.insert("cwd", cwd);
        }

        // initial_command applies when the worker is stateful AND we
        // don't yet have a session id to resume from.
        let for_initial = self.manifest.stateful && ctx.session_id.is_none();
        let argv = self.manifest.build_command(&values, for_initial);

        // `{prompt}` may live in argv (workers like `oz agent run
        // --prompt "..."`) or be expected on stdin (the way `claude
        // --print` consumes it). Detect per-invocation by scanning the
        // chosen template *before* substitution; if `{prompt}` was in
        // the template, the prompt is in argv and we close stdin to
        // avoid confusing workers that detect terminal-vs-pipe input.
        let template = if for_initial
            && self
                .manifest
                .initial_command
                .as_deref()
                .is_some_and(|t| !t.is_empty())
        {
            self.manifest.initial_command.as_deref().unwrap()
        } else {
            &self.manifest.command
        };
        let prompt_in_argv = template.iter().any(|arg| arg.contains("{prompt}"));

        if self.manifest.tty {
            self.invoke_pty(ctx, &argv, prompt_in_argv)
        } else {
            self.invoke_pipes(ctx, &argv, prompt_in_argv)
        }
    }
}

impl ManifestWorker {
    /// Spawn the worker with plain pipes (`Stdio::piped` / `Stdio::null`
    /// for stdin depending on whether the prompt is in argv). Keeps
    /// stdout / stderr separate so `session_id_capture { source =
    /// "stderr" }` works precisely.
    fn invoke_pipes(
        &self,
        ctx: &WorkerInvocation<'_>,
        argv: &[String],
        prompt_in_argv: bool,
    ) -> Result<WorkerResponse> {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(cwd) = ctx.cwd {
            cmd.current_dir(cwd);
        }
        // `JARVIS_VOICE_TURN=1` is the cross-cutting protocol signal
        // every Jarvis-spawned worker carries. Stop hooks observe it
        // to skip double-narration (see `feedback_stop_hook_recursion`
        // memory). Setting it here at the trait impl layer means
        // future built-in handlers and manifest workers both inherit
        // the behaviour without each having to remember to set it.
        cmd.env("JARVIS_VOICE_TURN", "1");
        cmd.stdin(if prompt_in_argv {
            Stdio::null()
        } else {
            Stdio::piped()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning worker {:?} ({})", self.manifest.id, argv[0]))?;

        if !prompt_in_argv {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| anyhow!("worker {:?} stdin unavailable", self.manifest.id))?;
            match stdin.write_all(ctx.prompt.as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // Worker exited before reading stdin — fine. This
                    // happens for stateless handlers whose command
                    // produces output without needing input (e.g. a
                    // hypothetical `time` handler that just prints the
                    // current clock), and was also showing up in CI
                    // as a flake against the `sh -c 'printf ...'`
                    // fixture used by the env-propagation test. The
                    // worker's output is still captured by
                    // `wait_with_output` below.
                    tracing::debug!(
                        worker = %self.manifest.id,
                        "worker exited before consuming stdin; continuing"
                    );
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "writing prompt to worker {:?} stdin",
                        self.manifest.id
                    )));
                }
            }
            // Closing stdin signals EOF so the worker doesn't wait
            // forever — safe even after a broken-pipe write.
            drop(child.stdin.take());
        }

        let out = child
            .wait_with_output()
            .with_context(|| format!("waiting on worker {:?}", self.manifest.id))?;
        if !out.status.success() {
            return Err(anyhow!(
                "worker {:?} exited with {}: {}",
                self.manifest.id,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let captured_session_id = self.extract_session_id(&out.stdout, &out.stderr);

        Ok(WorkerResponse {
            text,
            captured_session_id,
        })
    }

    /// Spawn the worker inside a pseudo-terminal. Required for
    /// interactive CLIs (`oz`, interactive `gemini-cli`, etc.) that
    /// detect a non-TTY stdin/stdout and either buffer output weirdly
    /// or refuse to run.
    ///
    /// PTY trade-off: stdout and stderr share the same TTY device, so
    /// `session_id_capture::source` is functionally a hint here — the
    /// regex matches against the *combined* output regardless of
    /// whether the manifest specified `stdout` or `stderr`. Workers
    /// whose session id can only be discriminated by stream should
    /// stick with `tty = false` (the default).
    fn invoke_pty(
        &self,
        ctx: &WorkerInvocation<'_>,
        argv: &[String],
        prompt_in_argv: bool,
    ) -> Result<WorkerResponse> {
        use std::io::Read as _;

        use portable_pty::{CommandBuilder, PtySize, native_pty_system};

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .with_context(|| format!("opening pty for worker {:?}", self.manifest.id))?;

        let mut builder = CommandBuilder::new(&argv[0]);
        for arg in &argv[1..] {
            builder.arg(arg);
        }
        if let Some(cwd) = ctx.cwd {
            builder.cwd(cwd);
        }
        builder.env("JARVIS_VOICE_TURN", "1");

        let mut child = pair
            .slave
            .spawn_command(builder)
            .with_context(|| format!("spawning worker {:?} ({}) via pty", self.manifest.id, argv[0]))?;
        // Drop the slave so its file descriptors close on our side;
        // the child still holds its half, which keeps the pty alive
        // until the child exits.
        drop(pair.slave);

        // Take a reader BEFORE the writer — `take_writer` consumes the
        // master in some implementations, leaving no way back for a
        // reader. Cloning the reader first is the safe order.
        let mut reader = pair
            .master
            .try_clone_reader()
            .with_context(|| format!("cloning pty reader for worker {:?}", self.manifest.id))?;

        if !prompt_in_argv {
            let mut writer = pair
                .master
                .take_writer()
                .with_context(|| format!("taking pty writer for worker {:?}", self.manifest.id))?;
            writer
                .write_all(ctx.prompt.as_bytes())
                .with_context(|| format!("writing prompt to worker {:?} pty", self.manifest.id))?;
            // Drop the writer so its descriptor closes — for line-
            // buffered consumers that's the EOF signal they need.
            drop(writer);
        }

        // Drop the master after the child has its slave half. The
        // reader was cloned above so it still works; keeping the
        // master handle alive forever would prevent the reader from
        // ever seeing EOF when the child exits.
        drop(pair.master);

        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .with_context(|| format!("reading pty output for worker {:?}", self.manifest.id))?;

        let status = child
            .wait()
            .with_context(|| format!("waiting on worker {:?} pty child", self.manifest.id))?;
        if !status.success() {
            return Err(anyhow!(
                "worker {:?} (pty) exited with {:?}: {}",
                self.manifest.id,
                status.exit_code(),
                String::from_utf8_lossy(&output).trim()
            ));
        }

        let text = String::from_utf8_lossy(&output).trim().to_string();
        // PTY merges stdout and stderr, so the capture regex runs
        // against the combined stream regardless of the manifest's
        // `source` setting. Documented in `invoke_pty`'s rustdoc.
        let captured_session_id = self.extract_session_id(&output, &output);

        Ok(WorkerResponse {
            text,
            captured_session_id,
        })
    }
}

impl ManifestWorker {
    fn extract_session_id(&self, stdout: &[u8], stderr: &[u8]) -> Option<String> {
        let cap = self.manifest.session_id_capture.as_ref()?;
        let regex = self.capture_regex.as_ref()?;
        let bytes = match cap.source {
            SessionIdSource::Stdout => stdout,
            SessionIdSource::Stderr => stderr,
        };
        let s = String::from_utf8_lossy(bytes);
        regex
            .captures(&s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workers::manifest::SessionIdCapture;

    fn invoke(worker: &dyn WorkerHandle, prompt: &str, session_id: Option<&str>) -> WorkerResponse {
        worker
            .invoke(&WorkerInvocation {
                prompt,
                session_id,
                cwd: None,
            })
            .expect("invoke succeeded")
    }

    /// Stateless worker with `{prompt}` in argv: `sh -c 'echo {prompt}'`
    /// substitutes the prompt into the shell command and the worker
    /// prints it back. Verifies the argv-placeholder code path including
    /// the "stdin is null when prompt is in argv" branch.
    #[test]
    fn manifest_worker_with_prompt_in_argv() {
        let manifest = WorkerManifest {
            id: "echo".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf %s '{prompt}'".to_string(),
            ],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "hola mundo", None);
        assert_eq!(resp.text, "hola mundo");
        assert!(resp.captured_session_id.is_none());
    }

    /// Stateless worker without `{prompt}` in argv: `cat` reads stdin
    /// and writes it back. Verifies the prompt-via-stdin code path.
    #[test]
    fn manifest_worker_with_prompt_via_stdin() {
        let manifest = WorkerManifest {
            id: "cat".to_string(),
            description: None,
            command: vec!["cat".to_string()],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "echoed via stdin", None);
        assert_eq!(resp.text, "echoed via stdin");
    }

    /// Stateful worker, first invocation (`session_id = None`): uses
    /// `initial_command`. Second invocation supplies a session id and
    /// uses `command` with substitution. Covers the dispatcher
    /// hand-off pattern that hija D will rely on.
    #[test]
    fn manifest_worker_picks_initial_vs_regular_command() {
        let manifest = WorkerManifest {
            id: "stateful".to_string(),
            description: None,
            // command embeds the session id so we can assert which
            // template ran.
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'cmd:%s' '{session_id}'".to_string(),
            ],
            initial_command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf init".to_string(),
            ]),
            stateful: true,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");

        let initial = invoke(&worker, "ignored", None);
        assert_eq!(initial.text, "init");

        let resumed = invoke(&worker, "ignored", Some("abc-123"));
        assert_eq!(resumed.text, "cmd:abc-123");
    }

    /// `session_id_capture` regex pulls the id out of stdout. Covers
    /// the bridge between worker output and the dispatcher's
    /// `active_workers` map.
    #[test]
    fn manifest_worker_captures_session_id_from_stdout() {
        let manifest = WorkerManifest {
            id: "stateful".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'Session: deadbeef-1234\\nreply text'".to_string(),
            ],
            initial_command: None,
            stateful: true,
            session_id_capture: Some(SessionIdCapture {
                source: SessionIdSource::Stdout,
                regex: r"Session: ([a-z0-9-]+)".to_string(),
            }),
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "ignored", None);
        assert_eq!(resp.captured_session_id.as_deref(), Some("deadbeef-1234"));
        // The capture text remains in stdout — the worker doesn't strip
        // it. That's the dispatcher's responsibility if it wants to.
        assert!(resp.text.contains("reply text"));
    }

    /// A worker exiting non-zero produces a useful error containing the
    /// worker id, exit status, and stderr snippet — matches the existing
    /// `claude.rs` failure shape so debugging stays uniform.
    #[test]
    fn manifest_worker_reports_nonzero_exit() {
        let manifest = WorkerManifest {
            id: "boom".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo something bad >&2; exit 7".to_string(),
            ],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let err = worker
            .invoke(&WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            })
            .expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("boom"), "got: {msg}");
        assert!(msg.contains("something bad"), "got: {msg}");
    }

    /// `JARVIS_VOICE_TURN=1` must reach every spawned worker. Stop
    /// hooks downstream observe this env var to skip themselves and
    /// avoid double-narration (see `feedback_stop_hook_recursion`
    /// memory). Without this contract the orchestrator would
    /// regress the fix shipped in spec 0007 the moment a worker
    /// runs through the registry instead of the bespoke
    /// `src/agents/claude.rs` env-setter.
    #[test]
    fn manifest_worker_env_carries_jarvis_voice_turn() {
        let manifest = WorkerManifest {
            id: "env-probe".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'JVT=%s' \"$JARVIS_VOICE_TURN\"".to_string(),
            ],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "ignored", None);
        assert_eq!(resp.text, "JVT=1");
    }

    /// `session_id_capture` with `source = "stderr"` reads from stderr
    /// instead of stdout. The plain-pipes path keeps the streams
    /// separate (unlike the PTY path), so this lets workers that emit
    /// their session id to stderr (some interactive CLIs do) work
    /// out of the box.
    #[test]
    fn manifest_worker_captures_session_id_from_stderr() {
        let manifest = WorkerManifest {
            id: "stderr-cap".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'Session: feed-face' >&2; printf reply".to_string(),
            ],
            initial_command: None,
            stateful: true,
            session_id_capture: Some(SessionIdCapture {
                source: SessionIdSource::Stderr,
                regex: r"Session: ([a-z0-9-]+)".to_string(),
            }),
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "ignored", None);
        assert_eq!(resp.text, "reply");
        assert_eq!(resp.captured_session_id.as_deref(), Some("feed-face"));
    }

    /// PTY path: a worker spawned with `tty = true` sees its stdin
    /// hooked to a real pseudo-terminal. The fixture command runs
    /// `tty` (which fails with "not a tty" on plain pipes and prints
    /// a device path on a PTY) plus echoes back stdin, so we can
    /// assert both halves: (a) `/dev/pts/...` or `/dev/ttyN` appears
    /// in the captured output, and (b) the prompt round-trips. Skips
    /// on hosts without `tty` and `cat` on PATH — every Unix-like CI
    /// runner has them, but the cargo-test-on-Windows future is a
    /// trap to leave for later.
    #[test]
    fn manifest_worker_tty_path_spawns_in_pseudo_terminal() {
        if which::which("tty").is_err() || which::which("cat").is_err() {
            eprintln!("skipping: tty/cat not on PATH");
            return;
        }
        let manifest = WorkerManifest {
            id: "tty-fixture".to_string(),
            description: None,
            // tty(1) tests stdin; cat then echoes stdin through. The
            // PTY makes both halves work; plain pipes would have tty
            // fail with "not a tty" on stderr.
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "tty; cat".to_string(),
            ],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: true,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "ping-tty-roundtrip", None);
        // tty(1) should have reported a device. /dev/pts is the
        // typical Linux/BSD path; /dev/ttyp* on some BSDs. Be
        // permissive about the prefix.
        assert!(
            resp.text.contains("/dev/"),
            "expected tty(1) to report a /dev/... device, got: {:?}",
            resp.text
        );
        // The prompt we wrote should be echoed back via cat.
        assert!(
            resp.text.contains("ping-tty-roundtrip"),
            "expected prompt round-trip, got: {:?}",
            resp.text
        );
    }

    /// PTY path with `{prompt}` in argv (no stdin write expected) —
    /// the worker reads its prompt from command-line args and the
    /// PTY's write side is never touched. Covers the orthogonal
    /// branch through `invoke_pty`.
    #[test]
    fn manifest_worker_tty_with_prompt_in_argv() {
        let manifest = WorkerManifest {
            id: "tty-argv".to_string(),
            description: None,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'got %s' '{prompt}'".to_string(),
            ],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: true,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let resp = invoke(&worker, "hola", None);
        assert!(
            resp.text.contains("got hola"),
            "expected prompt-in-argv path, got: {:?}",
            resp.text
        );
    }

    /// Non-zero exit through the PTY path produces an error string
    /// naming the worker. Mirrors the same contract the pipes path
    /// holds — debugging shapes stay uniform.
    #[test]
    fn manifest_worker_tty_reports_nonzero_exit() {
        let manifest = WorkerManifest {
            id: "tty-boom".to_string(),
            description: None,
            command: vec!["sh".to_string(), "-c".to_string(), "exit 9".to_string()],
            initial_command: None,
            stateful: false,
            session_id_capture: None,
            async_eligible: false,
            tty: true,
            dispatch_hint: None,
        };
        let worker =
            ManifestWorker::new(manifest, PathBuf::from("test.toml")).expect("compile worker");
        let err = worker
            .invoke(&WorkerInvocation {
                prompt: "",
                session_id: None,
                cwd: None,
            })
            .expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("tty-boom"), "got: {msg}");
        assert!(msg.contains("pty"), "expected pty branch in error: {msg}");
    }

    /// Construction fails if the capture regex doesn't compile. The
    /// registry relies on this to disable the worker rather than
    /// crashing.
    #[test]
    fn manifest_worker_construction_rejects_bad_regex() {
        let manifest = WorkerManifest {
            id: "broken".to_string(),
            description: None,
            command: vec!["sh".to_string()],
            initial_command: None,
            stateful: true,
            session_id_capture: Some(SessionIdCapture {
                source: SessionIdSource::Stdout,
                regex: "[unclosed".to_string(),
            }),
            async_eligible: false,
            tty: false,
            dispatch_hint: None,
        };
        let err = ManifestWorker::new(manifest, PathBuf::from("test.toml"))
            .expect_err("should fail at construction");
        assert!(format!("{err:#}").to_lowercase().contains("regex"));
    }
}
