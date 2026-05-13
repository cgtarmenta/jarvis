//! OpenAI / ChatGPT agent over the HTTP Chat Completions API.
//!
//! We use `ureq` rather than a heavyweight `async` HTTP stack. The pipeline
//! is single-threaded and serial — async buys us nothing here, and ureq
//! keeps the dependency tree small.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::{Agent, opt_f64, opt_string, require_env_or_opt};
use crate::session::Turn;

const DEFAULT_SYSTEM_PROMPT: &str = "You are a voice assistant. Reply in 1-3 short sentences unless the user \
     explicitly asks for detail. Plain prose — no markdown, no lists, no code \
     fences, since the reply will be spoken aloud.";

pub struct OpenAiAgent {
    api_key: String,
    base_url: String,
    model: String,
    system_prompt: String,
    temperature: f64,
}

impl OpenAiAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        let api_key = require_env_or_opt(&opts, "api_key", &["OPENAI_API_KEY"])?;
        let base_url = opt_string(&opts, "base_url", Some("https://api.openai.com/v1"))?
            .unwrap_or_else(|| "https://api.openai.com/v1".into());
        let model = opt_string(&opts, "model", Some("gpt-4o-mini"))?
            .unwrap_or_else(|| "gpt-4o-mini".into());
        let system_prompt = opt_string(&opts, "system_prompt", Some(DEFAULT_SYSTEM_PROMPT))?
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.into());
        let temperature = opt_f64(&opts, "temperature", 0.6)?;
        Ok(Self {
            api_key,
            base_url,
            model,
            system_prompt,
            temperature,
        })
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    temperature: f64,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

impl Agent for OpenAiAgent {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // Build a messages array with the system prompt up front, then
        // each prior turn in chronological order, then the current user
        // prompt. OpenAI handles the rest natively.
        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(Message {
            role: "system",
            content: &self.system_prompt,
        });
        for turn in history {
            messages.push(Message {
                role: turn.role.api_role(),
                content: &turn.content,
            });
        }
        messages.push(Message {
            role: "user",
            content: prompt,
        });
        let req = ChatRequest {
            model: &self.model,
            temperature: self.temperature,
            messages,
        };
        let body = ureq::post(&url)
            .set("authorization", &format!("Bearer {}", self.api_key))
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&req)?)
            .with_context(|| format!("POST {url}"))?;
        let parsed: ChatResponse = body.into_json().context("decoding OpenAI response")?;
        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow!("openai returned no choices"))
    }
}
