//! Claude Code agent — shells out to the `claude` CLI in print mode.
//!
//! Using the CLI rather than the API gives the user the full Claude Code
//! experience (tool use, sandboxed shell, file edits, MCP). For voice replies
//! we append a short system prompt that asks Claude to keep the answer
//! conversational and free of markdown — anything else sounds wrong over TTS.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tracing::warn;

use super::claude_attach::{self, Attachment};
use super::{Agent, opt_bool, opt_f64, opt_string, opt_string_vec};
use crate::session::Turn;

const DEFAULT_SYSTEM_PROMPT: &str = "You are a voice assistant. Reply concisely in 1-3 sentences unless the \
     user explicitly asks for detail. Avoid markdown — your reply will be \
     spoken aloud.";

pub struct ClaudeAgent {
    binary: String,
    system_prompt: Option<String>,
    extra_args: Vec<String>,
    cwd: Option<String>,
    /// `[agent].auto_resume` — at every turn pick the newest Claude
    /// session JSONL under `cwd`'s project namespace and pass it via
    /// `--resume`. The cache-file attachment (when present) overrides
    /// this entirely; see `claude_attach::resolve` for the priority.
    auto_resume: bool,
    timeout: Duration,
}

impl ClaudeAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        let binary =
            opt_string(&opts, "binary", Some("claude"))?.unwrap_or_else(|| "claude".into());
        let system_prompt = opt_string(&opts, "system_prompt", Some(DEFAULT_SYSTEM_PROMPT))?;
        let extra_args = opt_string_vec(&opts, "extra_args")?;
        let cwd = opt_string(&opts, "cwd", None)?;
        let auto_resume = opt_bool(&opts, "auto_resume", false)?;
        let timeout_secs = opt_f64(&opts, "timeout", 60.0)?;

        if which::which(&binary).is_err() {
            warn!(
                binary = %binary,
                "claude binary not found in PATH — agent will fail at runtime"
            );
        }
        Ok(Self {
            binary,
            system_prompt,
            extra_args,
            cwd,
            auto_resume,
            timeout: Duration::from_secs_f64(timeout_secs.max(1.0)),
        })
    }

    /// Resolve the active `Attachment` once per turn. The cache state
    /// file is consulted on every call (cheap; one fs::read) so the
    /// user's `jarvis claude attach` change applies immediately without
    /// restarting the daemon.
    fn current_attachment(&self) -> Attachment {
        let state = claude_attach::load_state().ok().flatten();
        claude_attach::resolve(state.as_ref(), self.cwd.as_deref(), self.auto_resume)
    }
}

impl Agent for ClaudeAgent {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--print");

        // Session resume: if attached (pinned or auto-latest), pass
        // `--resume <uuid>` so Claude Code loads the prior conversation
        // (tool calls, file edits, system messages — full fidelity).
        // We log which session is being resumed so the daemon log shows
        // it for the user.
        let attachment = self.current_attachment();
        if let Some(uuid) = attachment.to_uuid() {
            tracing::info!(session = %uuid, "claude --resume");
            cmd.args(["--resume", &uuid]);
        }

        if let Some(sp) = &self.system_prompt {
            cmd.args(["--append-system-prompt", sp]);
        }
        for a in &self.extra_args {
            cmd.arg(a);
        }
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }

        // Compose history into the prompt: claude --print is stateless per
        // invocation, so we embed prior turns as labelled "User:" /
        // "Assistant:" blocks and end with the current "User:" turn. The
        // model handles the conversational frame natively.
        let full_prompt = if history.is_empty() {
            prompt.to_string()
        } else {
            let mut buf = String::new();
            for turn in history {
                buf.push_str(turn.role.label());
                buf.push_str(": ");
                buf.push_str(&turn.content);
                buf.push_str("\n\n");
            }
            buf.push_str("User: ");
            buf.push_str(prompt);
            buf
        };

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {}", self.binary))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("claude stdin unavailable"))?
            .write_all(full_prompt.as_bytes())?;
        // Closing stdin signals EOF so claude doesn't wait forever.
        drop(child.stdin.take());

        // Rust's std::process doesn't have a built-in timeout. For an MVP we
        // wait synchronously; if claude hangs the user kills the daemon. A
        // follow-up can wrap this in `wait_timeout` if it becomes annoying.
        let _ = self.timeout;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(anyhow!(
                "claude exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
