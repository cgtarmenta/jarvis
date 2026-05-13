//! Warp `oz` agent — wraps `oz agent run --prompt …`.
//!
//! Warp's headless agent is shipped as the `oz` binary (`oz-preview` for the
//! preview channel). It runs autonomously, can hit MCP servers, can pick
//! among several models via `--model`, and authenticates non-interactively
//! via the `WARP_API_KEY` env var or an `--api-key` flag.
//!
//! Caveat: `oz agent run` streams tool calls and intermediate responses to
//! stdout as it works. There is no documented `--quiet` / `--json` mode.
//! For voice use we therefore capture stdout and return it as-is — users who
//! want a cleaner reply should configure a tight Warp **profile** (via
//! `profile = "..."` here) that restricts tool use, and/or steer the prompt
//! with `system_prompt` so the agent answers conversationally.

use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use tracing::warn;

use super::{Agent, opt_string, opt_string_vec};

const DEFAULT_SYSTEM_PROMPT: &str = "You are a voice assistant. Reply with a single short paragraph (1-3 \
     sentences) of plain prose, no markdown, no code fences. Do not run \
     shell tools unless explicitly required to answer.";

pub struct WarpAgent {
    binary: String,
    model: Option<String>,
    profile: Option<String>,
    cwd: Option<String>,
    api_key: Option<String>,
    system_prompt: Option<String>,
    extra_args: Vec<String>,
}

impl WarpAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        // Default to `oz`. If a user explicitly sets `binary = "..."` we
        // honour it; otherwise we fall through to `oz-preview` (Warp Preview
        // channel) and the deprecated `warp-cli` for legacy installs.
        let binary = if let Some(explicit) = opt_string(&opts, "binary", None)? {
            explicit
        } else {
            ["oz", "oz-preview", "warp-cli"]
                .into_iter()
                .find(|b| which::which(b).is_ok())
                .map(str::to_string)
                .unwrap_or_else(|| "oz".into())
        };

        if binary == "warp-cli" {
            warn!(
                "warp-cli is deprecated; Warp auto-updates it to `oz`. \
                 Update your installation when convenient."
            );
        }

        let model = opt_string(&opts, "model", None)?;
        let profile = opt_string(&opts, "profile", None)?;
        let cwd = opt_string(&opts, "cwd", None)?;
        // `api_key` may be set in config (less safe but explicit) or pulled
        // from $WARP_API_KEY by the `oz` binary itself — we don't require it
        // here because oz handles missing auth with its own error.
        let api_key = opt_string(&opts, "api_key", None)?;
        let system_prompt = opt_string(&opts, "system_prompt", Some(DEFAULT_SYSTEM_PROMPT))?;
        let extra_args = opt_string_vec(&opts, "extra_args")?;

        if which::which(&binary).is_err() {
            warn!(
                binary = %binary,
                "warp binary not found in PATH — install via Warp.app's \
                 Command Palette → \"Install Oz CLI Command\", or \
                 `brew install --cask oz` on macOS"
            );
        }
        Ok(Self {
            binary,
            model,
            profile,
            cwd,
            api_key,
            system_prompt,
            extra_args,
        })
    }

    /// Compose the final prompt sent to `oz`. We do not have access to
    /// `--system-prompt` on oz (unlike claude), so we prepend our voice-style
    /// instructions to the user's transcribed text. A blank line between them
    /// keeps the agent from concatenating them when summarizing back.
    fn compose_prompt(&self, user: &str) -> String {
        match &self.system_prompt {
            Some(sp) if !sp.trim().is_empty() => format!("{sp}\n\nUser: {user}"),
            _ => user.to_string(),
        }
    }
}

impl Agent for WarpAgent {
    fn name(&self) -> &'static str {
        "warp"
    }

    fn respond(&self, prompt: &str) -> Result<String> {
        let composed = self.compose_prompt(prompt);

        let mut cmd = Command::new(&self.binary);
        cmd.args(["agent", "run", "--prompt", &composed]);
        if let Some(m) = &self.model {
            cmd.args(["--model", m]);
        }
        if let Some(p) = &self.profile {
            cmd.args(["--profile", p]);
        }
        if let Some(c) = &self.cwd {
            cmd.args(["--cwd", c]);
        }
        if let Some(key) = &self.api_key {
            cmd.args(["--api-key", key]);
        }
        for a in &self.extra_args {
            cmd.arg(a);
        }
        // `oz` writes ANSI/color when stdout is a TTY but plain text when
        // it's piped (our case). We capture stdout to return upstream.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let out = cmd
            .output()
            .with_context(|| format!("spawning {}", self.binary))?;
        if !out.status.success() {
            return Err(anyhow!(
                "{} exited with {}: {}",
                self.binary,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
