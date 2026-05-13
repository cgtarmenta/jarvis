//! AI agent backends.
//!
//! An [`Agent`] takes the user's transcribed request and returns the text
//! Jarvis should speak back. Implementations are pure subprocess wrappers:
//! `claude`, `openai`, `gemini`, or any user-supplied `shell` command. Custom
//! plugins go through the `shell` agent — the user (or even Jarvis itself,
//! asked to write a plugin) drops a script that reads the prompt on stdin
//! and writes the reply on stdout.

use anyhow::{Context, Result, anyhow};

use crate::config::AgentConfig;

mod claude;
mod gemini;
mod openai;
mod shell;

pub use claude::ClaudeAgent;
pub use gemini::GeminiAgent;
pub use openai::OpenAiAgent;
pub use shell::ShellAgent;

pub trait Agent {
    fn name(&self) -> &'static str;
    fn respond(&self, prompt: &str) -> Result<String>;
}

/// Build the configured agent from `[agent]` block.
pub fn build(cfg: AgentConfig) -> Result<Box<dyn Agent + Send + Sync>> {
    let name = cfg.name.to_lowercase();
    match name.as_str() {
        "claude" | "claude-code" => Ok(Box::new(ClaudeAgent::from_options(cfg.options)?)),
        "openai" | "chatgpt" => Ok(Box::new(OpenAiAgent::from_options(cfg.options)?)),
        "gemini" | "google" => Ok(Box::new(GeminiAgent::from_options(cfg.options)?)),
        "shell" => Ok(Box::new(ShellAgent::from_options(cfg.options)?)),
        other => Err(anyhow!(
            "unknown agent: {other:?}. Built-ins: claude, openai, gemini, shell. \
             Use the shell agent to wire any custom CLI."
        )),
    }
}

/// Convenience: pull a string field from a `toml::Table`, returning the
/// default if absent. Centralised so the agents share a uniform error message
/// when a config value has the wrong type.
pub(crate) fn opt_string(
    opts: &toml::Table,
    key: &str,
    default: Option<&str>,
) -> Result<Option<String>> {
    match opts.get(key) {
        None => Ok(default.map(|s| s.to_string())),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(anyhow!(
            "agent option {key:?} must be a string, got {}",
            other.type_str()
        )),
    }
}

pub(crate) fn opt_f64(opts: &toml::Table, key: &str, default: f64) -> Result<f64> {
    match opts.get(key) {
        None => Ok(default),
        Some(toml::Value::Float(f)) => Ok(*f),
        Some(toml::Value::Integer(i)) => Ok(*i as f64),
        Some(other) => Err(anyhow!(
            "agent option {key:?} must be a number, got {}",
            other.type_str()
        )),
    }
}

pub(crate) fn opt_string_vec(opts: &toml::Table, key: &str) -> Result<Vec<String>> {
    match opts.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                toml::Value::String(s) => Ok(s.clone()),
                _ => Err(anyhow!("agent option {key:?} must be an array of strings")),
            })
            .collect(),
        Some(_) => Err(anyhow!("agent option {key:?} must be an array of strings")),
    }
}

/// Used by `OpenAI` / `Gemini` to short-circuit when the user has not set
/// the relevant API key.
pub(crate) fn require_env_or_opt(
    opts: &toml::Table,
    opt_key: &str,
    env_vars: &[&str],
) -> Result<String> {
    if let Some(toml::Value::String(s)) = opts.get(opt_key) {
        return Ok(s.clone());
    }
    for v in env_vars {
        if let Ok(value) = std::env::var(v) {
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    Err(anyhow!(
        "missing API key: set [agent].{opt_key} in config, or export one of: {}",
        env_vars.join(", ")
    ))
    .with_context(|| "agent requires an API key")
}
