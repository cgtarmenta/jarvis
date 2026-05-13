//! Shell-pipe agent — the universal escape hatch.
//!
//! Any command that reads the prompt on stdin and writes the reply on stdout
//! works as a Jarvis agent. This is how you wire Ollama, llama.cpp, Warp's
//! CLI, or a custom script — including a plugin Jarvis itself wrote when you
//! asked "hey jarvis, make me a plugin that controls my smart bulbs".

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

use super::{Agent, opt_string, opt_string_vec};

pub struct ShellAgent {
    command: Vec<String>,
    cwd: Option<String>,
}

impl ShellAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        let command = opt_string_vec(&opts, "command")?;
        if command.is_empty() {
            return Err(anyhow!(
                "shell agent requires a non-empty `command` array, e.g. command = [\"ollama\", \"run\", \"llama3\"]"
            ));
        }
        let cwd = opt_string(&opts, "cwd", None)?;
        Ok(Self { command, cwd })
    }
}

impl Agent for ShellAgent {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn respond(&self, prompt: &str) -> Result<String> {
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(c) = &self.cwd {
            cmd.current_dir(c);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning shell agent: {:?}", self.command[0]))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("shell agent stdin unavailable"))?
            .write_all(prompt.as_bytes())?;
        drop(child.stdin.take());

        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(anyhow!(
                "shell agent {:?} exited with {}: {}",
                self.command[0],
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
