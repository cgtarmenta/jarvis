//! Google Gemini agent over the HTTP REST API.
//!
//! Same pattern as the OpenAI agent: ureq for HTTP, serde for shaping the
//! request and response, plain-prose system prompt for TTS-friendly replies.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::{Agent, opt_string, require_env_or_opt};
use crate::session::{Role, Turn};

const DEFAULT_SYSTEM_PROMPT: &str = "You are a voice assistant. Reply in 1-3 short sentences. Plain prose, no \
     markdown — your reply will be read aloud.";

pub struct GeminiAgent {
    api_key: String,
    model: String,
    system_prompt: String,
}

impl GeminiAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        let api_key = require_env_or_opt(&opts, "api_key", &["GEMINI_API_KEY", "GOOGLE_API_KEY"])?;
        let model = opt_string(&opts, "model", Some("gemini-1.5-flash"))?
            .unwrap_or_else(|| "gemini-1.5-flash".into());
        let system_prompt = opt_string(&opts, "system_prompt", Some(DEFAULT_SYSTEM_PROMPT))?
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.into());
        Ok(Self {
            api_key,
            model,
            system_prompt,
        })
    }
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    contents: Vec<Content<'a>>,
    system_instruction: Content<'a>,
}

#[derive(Serialize)]
struct Content<'a> {
    /// `user` or `model` — Gemini's nomenclature (note: "model" not
    /// "assistant"). Optional in the API; omitted for system_instruction.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'a str>,
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Deserialize)]
struct GenerateResponse {
    candidates: Option<Vec<Candidate>>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<ContentOut>,
}

#[derive(Deserialize)]
struct ContentOut {
    parts: Option<Vec<PartOut>>,
}

#[derive(Deserialize)]
struct PartOut {
    text: Option<String>,
}

impl Agent for GeminiAgent {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        // Build a contents array from history + current prompt. Gemini
        // calls the assistant role "model" (not "assistant" like OpenAI).
        let mut contents = Vec::with_capacity(history.len() + 1);
        for turn in history {
            let role = match turn.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            contents.push(Content {
                role: Some(role),
                parts: vec![Part {
                    text: &turn.content,
                }],
            });
        }
        contents.push(Content {
            role: Some("user"),
            parts: vec![Part { text: prompt }],
        });
        let req = GenerateRequest {
            contents,
            system_instruction: Content {
                role: None,
                parts: vec![Part {
                    text: &self.system_prompt,
                }],
            },
        };
        let resp = ureq::post(&url)
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&req)?)
            .with_context(|| format!("POST gemini {}", self.model))?;
        let parsed: GenerateResponse = resp.into_json().context("decoding gemini response")?;
        let text = parsed
            .candidates
            .and_then(|cs| cs.into_iter().next())
            .and_then(|c| c.content)
            .and_then(|c| c.parts)
            .and_then(|p| p.into_iter().next())
            .and_then(|p| p.text)
            .ok_or_else(|| anyhow!("gemini returned no text"))?;
        Ok(text.trim().to_string())
    }
}
