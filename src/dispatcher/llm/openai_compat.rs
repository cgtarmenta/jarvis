//! OpenAI-compatible chat-completions backend — spec 0013 / B-2.
//!
//! Speaks the OpenAI Chat Completions wire protocol over plain HTTP +
//! JSON. The exact same shape (request body + response shape) is
//! implemented by ~every modern inference frontend: OpenAI itself,
//! Anthropic's OpenAI-compat shim, Groq, Fireworks, Together,
//! Mistral's API, locally-hosted Ollama (with the `/v1/...` prefix),
//! vLLM, llama.cpp's server, and — relevant here — Nvidia Triton's
//! OpenAI-compatible endpoint that the user's GB200 cluster exposes
//! (see `project_user_gpu_infrastructure` memory).
//!
//! Wire contract this backend assumes:
//!
//! - POST `<endpoint>` (caller supplies the full URL including
//!   `/chat/completions`) with JSON body `{ model, messages: [...],
//!   temperature, max_tokens }`.
//! - 200 reply is a JSON object whose
//!   `.choices[0].message.content` is the model's text reply.
//! - Optional `Authorization: Bearer <api_key>` header.
//! - Optional extra headers (X-VPN-Route, X-Project-Id, custom auth
//!   tokens) for users routing through gateways.
//!
//! Any failure — network, non-200, malformed JSON, missing content
//! field — becomes a backend `Err`. The cascade adapter (B-4)
//! catches that and falls through to stage 3 without bothering the
//! user. Speed > precision is the project-level rule.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::json;

use super::{LlmBackend, WorkerInfo, default_classifier_prompt, parse_worker_id};

/// Default per-call timeout. Matches the spec's "default 5s" line.
/// Per-call rather than per-connection because the model may take
/// most of those seconds *thinking*; the TCP handshake is fast.
const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Default classifier sampling cap. The reply we want is "the worker
/// id, alone on the first line" — a handful of tokens, never long.
/// Capping at 32 keeps latency tight even on chatty models and is
/// generous enough for the longest worker id Jarvis could plausibly
/// have (e.g. `task-list`, `session-reset`).
const DEFAULT_MAX_TOKENS: u32 = 32;

/// HTTP-based OpenAI Chat Completions classifier.
///
/// Construct via [`OpenAiCompatBackend::new`]; configuration is
/// immutable once built. The backend owns no I/O state between calls
/// (each `classify` opens a fresh ureq agent so an unhealthy
/// connection from one turn doesn't poison the next).
pub struct OpenAiCompatBackend {
    endpoint: String,
    model: String,
    api_key: Option<String>,
    headers: HashMap<String, String>,
    timeout: Duration,
}

impl OpenAiCompatBackend {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: None,
            headers: HashMap::new(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl LlmBackend for OpenAiCompatBackend {
    fn name(&self) -> &str {
        "openai_compat"
    }

    fn classify(&self, prompt: &str, workers: &[WorkerInfo]) -> Result<Option<String>> {
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "user", "content": default_classifier_prompt(prompt, workers) }
            ],
            // Greedy decode: routing is a classification, not a
            // creative task — `temperature = 0` keeps the same
            // prompt producing the same answer turn over turn,
            // which also makes the 60s in-memory cache (B-4) a
            // pure win instead of a "sometimes-wrong" optimisation.
            "temperature": 0,
            "max_tokens": DEFAULT_MAX_TOKENS,
        });

        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let mut req = agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json");
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        for (name, value) in &self.headers {
            req = req.set(name, value);
        }

        let response = req
            .send_json(body)
            .with_context(|| format!("POST {} (classifier call)", self.endpoint))?;

        // ureq treats 4xx/5xx as Err on `send_json`, so anything we
        // get here is a 2xx — but other endpoints sometimes return
        // 200 with an error body. We treat malformed/empty content
        // as a backend error so the cascade adapter falls through.
        let json: serde_json::Value = response
            .into_json()
            .context("decoding classifier response as JSON")?;
        let content = extract_message_content(&json).ok_or_else(|| {
            anyhow!(
                "classifier response missing .choices[0].message.content: {}",
                json
            )
        })?;
        Ok(parse_worker_id(&content))
    }
}

/// Pull `choices[0].message.content` out of the response, accepting
/// both the plain-string shape (OpenAI/Groq/Anthropic-compat) and
/// the array-of-parts shape that a few servers (e.g. some vLLM
/// builds with multimodal extensions) emit. Returns `None` only when
/// neither shape produces a string — letting the caller emit a
/// useful error pointing at the actual JSON.
fn extract_message_content(root: &serde_json::Value) -> Option<String> {
    let msg = root.get("choices")?.get(0)?.get("message")?;
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    // Array-of-parts: `[{ "type": "text", "text": "..." }, ...]`.
    // Concatenate all text segments; non-text parts (unlikely for a
    // classifier reply) are skipped.
    if let Some(arr) = content.as_array() {
        let mut buf = String::new();
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                buf.push_str(t);
            }
        }
        if !buf.is_empty() {
            return Some(buf);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A single-request mock server: binds an ephemeral port, accepts
    /// one connection, reads the request, sends `response_body` as the
    /// JSON 200 reply, then closes. Returns `(url, captured_request)`
    /// where `captured_request` is the full raw request text — useful
    /// for asserting on headers and the body the backend sent.
    fn one_shot_server(response_body: String) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(p) => p,
                Err(_) => return,
            };
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            // Read until we see the end of headers + body. Naive but
            // sufficient: requests from ureq are < 4KB for our cases.
            let mut buf = [0u8; 8192];
            let mut captured = Vec::new();
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        captured.extend_from_slice(&buf[..n]);
                        // Heuristic: stop reading once we have headers
                        // + a content-length-sized body. Real classifiers
                        // POST one shot, so we don't need a real parser.
                        let text = String::from_utf8_lossy(&captured);
                        if let Some(idx) = text.find("\r\n\r\n") {
                            let header = &text[..idx];
                            let cl = header
                                .lines()
                                .find_map(|l| {
                                    let l = l.to_ascii_lowercase();
                                    l.strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            let body_so_far = captured.len() - (idx + 4);
                            if body_so_far >= cl {
                                break;
                            }
                        }
                        if n < buf.len() {
                            // Probably done — give a tiny grace by
                            // looping again only if read had data.
                            // Falls through naturally on the next 0.
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&captured).to_string());

            let body = response_body.as_bytes();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        });

        (url, rx)
    }

    fn one_shot_server_status(status: u16, response_body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");

        thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(p) => p,
                Err(_) => return,
            };
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);

            let reason = match status {
                400 => "Bad Request",
                401 => "Unauthorized",
                500 => "Internal Server Error",
                _ => "Status",
            };
            let body = response_body.as_bytes();
            let header = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        });

        url
    }

    fn worker(id: &str, hint: Option<&str>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            dispatch_hint: hint.map(|s| s.to_string()),
        }
    }

    /// Happy path: server returns a well-formed OpenAI reply, the
    /// backend pulls the worker id out, and the request body we
    /// actually sent includes the model + a user message carrying
    /// the workers list + transcript.
    #[test]
    fn classify_extracts_worker_id_from_well_formed_reply() {
        let (url, rx) = one_shot_server(
            r#"{
              "choices": [
                { "message": { "role": "assistant", "content": "time" } }
              ]
            }"#
            .to_string(),
        );

        let backend = OpenAiCompatBackend::new(url, "llama-3.1-8b-instruct")
            .with_api_key("sk-test")
            .with_header("X-Custom", "yes");
        let result = backend
            .classify(
                "qué hora es en Tokio",
                &[
                    worker("time", Some("Clock queries.")),
                    worker("claude", None),
                ],
            )
            .unwrap();
        assert_eq!(result.as_deref(), Some("time"));

        let captured = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server saw request");
        // Authorization + custom header round-trip.
        assert!(
            captured.contains("Authorization: Bearer sk-test")
                || captured.contains("authorization: Bearer sk-test"),
            "request missing bearer: {captured}"
        );
        assert!(
            captured.contains("X-Custom: yes") || captured.contains("x-custom: yes"),
            "request missing custom header: {captured}"
        );
        // Body includes the model and the transcript embedded in the
        // user message.
        assert!(
            captured.contains("llama-3.1-8b-instruct"),
            "body missing model: {captured}"
        );
        assert!(
            captured.contains("qué hora es en Tokio") || captured.contains("Tokio"),
            "body missing transcript: {captured}"
        );
        // Body lists workers + hint so the classifier has context.
        assert!(
            captured.contains("time") && captured.contains("Clock queries."),
            "body missing worker hint: {captured}"
        );
    }

    /// `none` reply maps to `Ok(None)` so the cascade falls through.
    #[test]
    fn classify_decline_yields_none() {
        let (url, _rx) = one_shot_server(
            r#"{"choices":[{"message":{"role":"assistant","content":"none"}}]}"#.to_string(),
        );
        let backend = OpenAiCompatBackend::new(url, "m");
        let result = backend
            .classify("anything", &[worker("time", None)])
            .unwrap();
        assert!(result.is_none());
    }

    /// Chatty wrappers around the id still resolve.
    #[test]
    fn classify_tolerates_chatty_replies() {
        let (url, _rx) = one_shot_server(
            r#"{"choices":[{"message":{"role":"assistant","content":"`task-list`\nThat's the best match."}}]}"#
                .to_string(),
        );
        let backend = OpenAiCompatBackend::new(url, "m");
        let result = backend
            .classify("qué tareas tengo", &[worker("task-list", None)])
            .unwrap();
        assert_eq!(result.as_deref(), Some("task-list"));
    }

    /// Array-of-parts content (some vLLM/multimodal builds) is
    /// concatenated. Real chat models give a string; this exercises
    /// the fallback so a server upgrade doesn't silently break
    /// routing.
    #[test]
    fn classify_accepts_array_of_parts_content() {
        let (url, _rx) = one_shot_server(
            r#"{"choices":[{"message":{"role":"assistant","content":[
                {"type":"text","text":"time"},
                {"type":"text","text":"\n"}
            ]}}]}"#
                .to_string(),
        );
        let backend = OpenAiCompatBackend::new(url, "m");
        let result = backend.classify("hora", &[worker("time", None)]).unwrap();
        assert_eq!(result.as_deref(), Some("time"));
    }

    /// HTTP 500 → backend error. The cascade adapter (B-4) is what
    /// turns this into a stage-3 fallthrough; the trait itself
    /// propagates so the caller can choose.
    #[test]
    fn classify_propagates_5xx_as_error() {
        let url = one_shot_server_status(500, r#"{"error":"upstream offline"}"#.to_string());
        let backend = OpenAiCompatBackend::new(url, "m");
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("5xx should error");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("classifier") || msg.contains("/v1/chat/completions"),
            "error should mention the call site: {msg}"
        );
    }

    /// Malformed response (200 OK but missing `choices[0].message.content`)
    /// → backend error. Mirrors what some local servers return when
    /// rate-limited or warming up.
    #[test]
    fn classify_errors_on_missing_content() {
        let (url, _rx) = one_shot_server(r#"{"choices":[{"message":{}}]}"#.to_string());
        let backend = OpenAiCompatBackend::new(url, "m");
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("missing content should error");
        assert!(
            format!("{err:#}").contains("missing"),
            "error should mention missing content"
        );
    }

    /// Configured timeout actually applies. Bind a listener that
    /// never accepts and confirm we error out within ~timeout. Uses
    /// a small timeout so the test is fast.
    #[test]
    fn classify_honors_configured_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        // Keep the listener alive but never accept — ureq will hang
        // on connect/read until timeout.
        let _keep = listener;

        let backend = OpenAiCompatBackend::new(url, "m").with_timeout(Duration::from_millis(200));
        let start = std::time::Instant::now();
        let err = backend
            .classify("anything", &[worker("time", None)])
            .expect_err("should timeout");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "should respect ~200ms timeout, took {elapsed:?}"
        );
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("timed out") || msg.contains("timeout") || msg.contains("classifier"),
            "expected timeout-shaped error, got: {msg}"
        );
    }

    /// `name()` returns the stable identifier used by tracing log
    /// fields and `jarvis dispatcher status`.
    #[test]
    fn name_is_stable() {
        let backend = OpenAiCompatBackend::new("http://x", "m");
        assert_eq!(backend.name(), "openai_compat");
    }

    /// `extract_message_content` returns `None` for empty/malformed
    /// shapes — a separate unit test so the helper's edge cases are
    /// covered without spinning a server.
    #[test]
    fn extract_message_content_handles_edge_cases() {
        assert_eq!(extract_message_content(&serde_json::json!({})), None);
        assert_eq!(
            extract_message_content(&serde_json::json!({"choices": []})),
            None
        );
        assert_eq!(
            extract_message_content(&serde_json::json!({"choices":[{"message":{"content":""}}]}))
                .as_deref(),
            Some("")
        );
        assert_eq!(
            extract_message_content(
                &serde_json::json!({"choices":[{"message":{"content":[{"type":"text","text":"hi"}]}}]})
            )
            .as_deref(),
            Some("hi")
        );
    }
}
