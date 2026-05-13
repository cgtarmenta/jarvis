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

use super::{Agent, opt_f64, opt_string, opt_string_vec};
use crate::session::Turn;

const DEFAULT_SYSTEM_PROMPT: &str = "You are a voice assistant. Reply concisely in 1-3 sentences unless the \
     user explicitly asks for detail. Avoid markdown — your reply will be \
     spoken aloud.";

pub struct ClaudeAgent {
    binary: String,
    system_prompt: Option<String>,
    extra_args: Vec<String>,
    cwd: Option<String>,
    timeout: Duration,
}

impl ClaudeAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        let binary =
            opt_string(&opts, "binary", Some("claude"))?.unwrap_or_else(|| "claude".into());
        let system_prompt = opt_string(&opts, "system_prompt", Some(DEFAULT_SYSTEM_PROMPT))?;
        let extra_args = opt_string_vec(&opts, "extra_args")?;
        let cwd = opt_string(&opts, "cwd", None)?;
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
            timeout: Duration::from_secs_f64(timeout_secs.max(1.0)),
        })
    }
}

impl Agent for ClaudeAgent {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--print");
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
