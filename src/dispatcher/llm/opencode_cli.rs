//! Subprocess-driven classifier backed by the `opencode` CLI —
//! spec 0016.
//!
//! Same shape as [`super::oz_cli::OzCliBackend`]: spawn a
//! subprocess, stream NDJSON from stdout, filter the events that
//! carry the model's reply, hand the joined text to
//! [`super::parse_worker_id`].
//!
//! `opencode` is added in spec 0016 because (per the 2026-05-15
//! benchmark) its free-tier models reply in ~3s with correct,
//! parseable output — by a wide margin the fastest CLI-agent
//! classifier on the user's machine. Users without their own
//! OpenAI-compat endpoint (Triton / Ollama / etc.) get a stage-2
//! default that's actually voice-cascade fast.
//!
//! Wire contract:
//!
//! - Binary is named `opencode` by default, resolvable on PATH;
//!   override via [`OpencodeCliBackend::with_binary`] for custom
//!   installs.
//! - Argv: `opencode run --format json -m <provider/model>
//!   <prompt>`. The prompt comes through as a single argv element.
//! - Stdout is NDJSON. We care about events with top-level
//!   `type == "text"`; their model reply lives at `part.text`. We
//!   concatenate `part.text` across every such event in order.
//! - If no `text` events surfaced before exit, return `Ok(None)`
//!   — the documented decline path.
//! - Non-zero exit code → backend error with the stderr snippet,
//!   same envelope as the oz backend.
//! - Auth is handled by `opencode` itself (login store on disk).
//!   We don't plumb api_keys, mirroring how `oz` is handled.

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

/// Default per-call timeout. Measured P50 ≈ 3s, P95 < 5s across
/// opencode's free-tier models (qwen3.6-plus-free,
/// deepseek-v4-flash-free, big-pickle) on 2026-05-15. 15s gives
/// 3x P95 headroom — generous without locking the cascade out
/// for unbounded time. Override via `timeout_secs` in
/// `[dispatcher.fallback]`.
const DEFAULT_TIMEOUT_SECS: u64 = 15;

/// `opencode` subprocess classifier.
///
/// Construct via [`OpencodeCliBackend::new`] with a `provider/model`
/// id (opencode's native shape, e.g. `opencode/qwen3.6-plus-free`).
/// Chainable setters override the binary path or timeout before
/// installing as the cascade's stage-2 backend.
pub struct OpencodeCliBackend {
    binary: String,
    model: String,
    timeout: Duration,
}

impl OpencodeCliBackend {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            binary: "opencode".to_string(),
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

impl LlmBackend for OpencodeCliBackend {
    fn name(&self) -> &str {
        "opencode"
    }

    fn classify(&self, prompt: &str, workers: &[WorkerInfo]) -> Result<Option<String>> {
        let classifier_prompt = default_classifier_prompt(prompt, workers);

        // Argv mirrors what `opencode run --help` documents. We pass
        // the prompt as a positional `message` argument (single argv
        // element so embedded newlines / quotes don't break the call,
        // same convention oz_cli uses).
        //
        // Process-group setup on unix matches the oz backend's
        // rationale: `opencode` may fork helper processes; SIGTERM-ing
        // only the parent leaves children holding the stdout pipe
        // open. Killing the group brings everything down at once.
        let mut cmd = Command::new(&self.binary);
        cmd.args([
            "run",
            "--format",
            "json",
            "-m",
            &self.model,
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
            timed_out_for_timer.store(true, Ordering::Relaxed);
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        });

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
        match extract_text_events(&stdout) {
            Some(reply) => Ok(parse_worker_id(&reply)),
            None => Ok(None),
        }
    }
}

/// Extract the model's reply from `opencode run --format json`
/// NDJSON output.
///
/// The stream's relevant shape (captured 2026-05-15):
/// ```text
/// {"type":"step_start","part":{...}}
/// {"type":"text","part":{"type":"text","text":"claude","time":{...}}}
/// {"type":"step_finish","part":{"tokens":{...},"cost":...}}
/// ```
///
/// We filter top-level `type == "text"` events and concatenate
/// `part.text` across all of them. Other event types are ignored.
/// Empty stream or no `text` events → `None` (cascade falls
/// through to stage 3).
///
/// Malformed lines are silently skipped; opencode occasionally
/// prefixes its stdout with non-JSON banners under
/// `--print-logs`-adjacent flags, and we don't want to abort the
/// parse over them.
pub(crate) fn extract_text_events(stdout: &str) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(event_type) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if event_type != "text" {
            continue;
        }
        // The opencode shape nests the reply under `part.text`.
        // Defensive: fall back to top-level `text` in case a future
        // opencode version flattens the shape.
        let text = value
            .get("part")
            .and_then(|p| p.get("text"))
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()));
        if let Some(t) = text {
            chunks.push(t.to_string());
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.concat())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// Write a `#!/bin/sh` fixture script (mode 0755) inside `dir`.
    /// Mirrors `oz_cli::tests::fixture` — same ETXTBSY race guard.
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
        std::thread::sleep(Duration::from_millis(20));
        p
    }

    fn worker(id: &str, hint: Option<&str>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            dispatch_hint: hint.map(|s| s.to_string()),
        }
    }

    fn json_stream_script(events: &[&str]) -> String {
        let mut body = String::new();
        for e in events {
            body.push_str(&format!("echo '{e}'\n"));
        }
        body
    }

    /// Happy path: real-shape NDJSON stream with step_start,
    /// text (reply), step_finish. Backend extracts the text and
    /// parses the worker id.
    #[test]
    fn classify_extracts_worker_id_from_text_event() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "opencode-mock",
            &json_stream_script(&[
                r#"{"type":"step_start","timestamp":1,"sessionID":"s","part":{"type":"step-start"}}"#,
                r#"{"type":"text","timestamp":2,"sessionID":"s","part":{"type":"text","text":"time"}}"#,
                r#"{"type":"step_finish","timestamp":3,"sessionID":"s","part":{"reason":"stop","tokens":{"total":10}}}"#,
            ]),
        );

        let backend = OpencodeCliBackend::new("opencode/qwen3.6-plus-free")
            .with_binary(bin.to_string_lossy());
        let result = backend
            .classify("qué hora es", &[worker("time", Some("Clock queries."))])
            .unwrap();
        assert_eq!(result.as_deref(), Some("time"));
    }

    /// Streamed reply across multiple `text` events: concatenated
    /// in arrival order before parse_worker_id sees it.
    #[test]
    fn classify_concatenates_multiple_text_events() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "opencode-mock",
            &json_stream_script(&[
                r#"{"type":"step_start","part":{}}"#,
                r#"{"type":"text","part":{"text":"task"}}"#,
                r#"{"type":"text","part":{"text":"-list"}}"#,
                r#"{"type":"step_finish","part":{}}"#,
            ]),
        );

        let backend = OpencodeCliBackend::new("opencode/qwen3.6-plus-free")
            .with_binary(bin.to_string_lossy());
        let result = backend
            .classify("qué tareas tengo", &[worker("task-list", None)])
            .unwrap();
        assert_eq!(result.as_deref(), Some("task-list"));
    }

    /// No `text` events surfaced (e.g. opencode session exited
    /// without emitting a reply) → Ok(None). Cascade falls
    /// through to stage 3.
    #[test]
    fn classify_yields_none_when_no_text_events() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "opencode-mock",
            &json_stream_script(&[
                r#"{"type":"step_start","part":{}}"#,
                r#"{"type":"step_finish","part":{}}"#,
            ]),
        );

        let backend = OpencodeCliBackend::new("opencode/qwen3.6-plus-free")
            .with_binary(bin.to_string_lossy());
        let result = backend
            .classify("anything", &[worker("time", None)])
            .unwrap();
        assert!(result.is_none(), "no text events → should decline");
    }

    /// Non-zero exit → backend error including the stderr snippet,
    /// matching the oz backend's envelope.
    #[test]
    fn classify_propagates_nonzero_exit_as_error() {
        let tmp = TempDir::new().unwrap();
        let bin = fixture(
            tmp.path(),
            "opencode-mock",
            "echo 'opencode: not authenticated' >&2\nexit 3",
        );

        let backend = OpencodeCliBackend::new("opencode/qwen3.6-plus-free")
            .with_binary(bin.to_string_lossy());
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("nonzero exit should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("classifier exited"), "got: {msg}");
        assert!(msg.contains("not authenticated"), "got: {msg}");
    }

    /// `name()` returns the stable identifier used by tracing.
    #[test]
    fn name_is_stable() {
        let backend = OpencodeCliBackend::new("opencode/qwen3.6-plus-free");
        assert_eq!(backend.name(), "opencode");
    }

    // ---------- pure-function parser tests ----------------------

    /// Real-captured shape from `opencode run --format json` on
    /// 2026-05-15. Locks the parser contract without spawning.
    #[test]
    fn extract_text_events_real_world_shape() {
        let stdout = r#"{"type":"step_start","timestamp":1778856973944,"sessionID":"ses_xxx","part":{"id":"prt_xxx","messageID":"msg_xxx","sessionID":"ses_xxx","snapshot":"d04...","type":"step-start"}}
{"type":"text","timestamp":1778856973957,"sessionID":"ses_xxx","part":{"id":"prt_yyy","messageID":"msg_xxx","sessionID":"ses_xxx","type":"text","text":"claude","time":{"start":1778856973944,"end":1778856973956}}}
{"type":"step_finish","timestamp":1778856973967,"sessionID":"ses_xxx","part":{"id":"prt_zzz","reason":"stop","snapshot":"d04...","messageID":"msg_xxx","sessionID":"ses_xxx","type":"step-finish","tokens":{"total":11283,"input":11280,"output":3,"reasoning":0,"cache":{"write":0,"read":0}},"cost":0.0033867}}
"#;
        assert_eq!(extract_text_events(stdout), Some("claude".to_string()));
    }

    #[test]
    fn extract_text_events_concatenates_in_order() {
        let stdout = r#"{"type":"step_start","part":{}}
{"type":"text","part":{"text":"hello "}}
{"type":"text","part":{"text":"world"}}
"#;
        assert_eq!(extract_text_events(stdout), Some("hello world".to_string()));
    }

    #[test]
    fn extract_text_events_falls_back_to_top_level_text() {
        // Defensive: if opencode flattens the shape in the future
        // and puts `text` at the top level, we still handle it.
        let stdout = r#"{"type":"text","text":"time"}"#;
        assert_eq!(extract_text_events(stdout), Some("time".to_string()));
    }

    #[test]
    fn extract_text_events_returns_none_on_empty_or_no_text() {
        assert_eq!(extract_text_events(""), None);
        assert_eq!(extract_text_events("\n\n\n"), None);
        assert_eq!(
            extract_text_events(
                r#"{"type":"step_start","part":{}}
{"type":"step_finish","part":{}}
"#
            ),
            None
        );
    }

    #[test]
    fn extract_text_events_tolerates_malformed_lines() {
        let stdout =
            "banner not json\n{also not json}\n{\"type\":\"text\",\"part\":{\"text\":\"time\"}}\n";
        assert_eq!(extract_text_events(stdout), Some("time".to_string()));
    }
}
