//! LLM dispatcher — stage 2 of the cascade, spec 0013 (orchestrator B).
//!
//! When the deterministic intent matchers in stage 1 decline a turn,
//! this stage hands the transcript to a pluggable [`LlmBackend`] along
//! with the registry's worker manifest summaries. The backend returns
//! either a worker id (route the prompt there) or `None` (no clear
//! pick — let stage 3 take over with the default worker).
//!
//! This module ships the trait, the [`WorkerInfo`] descriptor it
//! consumes, the default classifier prompt builder, and one concrete
//! backend per file in the `llm/` submodule directory
//! ([`openai_compat::OpenAiCompatBackend`] in B-2, an `oz`-cli wrapper
//! in B-3). The cascade integration that wraps a backend as a
//! [`super::Dispatcher`] lands in B-4.
//!
//! The trait deliberately returns *just* the chosen worker id, not a
//! full [`super::DispatchDecision`]:
//!
//! - The backend has no way to know whether the chosen worker is
//!   stateful or what its prior session id is — that needs the
//!   registry + session, which only the cascade adapter has.
//! - Returning a string keeps backends testable as pure functions of
//!   `(prompt, workers)` without leaking dispatcher internals.
//! - The adapter validates the id against the live registry; a
//!   hallucinated id from the model becomes `None` and the cascade
//!   falls through gracefully.

use std::sync::Arc;

use anyhow::Result;

use crate::workers::{WorkerHandle, WorkerRegistry};

pub mod openai_compat;

pub use openai_compat::OpenAiCompatBackend;

/// A minimal snapshot of a worker for the classifier prompt. Only the
/// fields the LLM actually needs to make a routing decision: the id
/// (so we can match its reply back to a registered worker) and the
/// `dispatch_hint` (so the model has natural-language guidance for
/// when this worker is the right pick).
#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub id: String,
    pub dispatch_hint: Option<String>,
}

impl WorkerInfo {
    pub fn from_handle(h: &Arc<dyn WorkerHandle>) -> Self {
        Self {
            id: h.id().to_string(),
            dispatch_hint: h.dispatch_hint().map(|s| s.to_string()),
        }
    }

    /// Snapshot every active worker in the registry. Used by the
    /// cascade adapter once per turn so the LLM always sees the
    /// current set, including manifest workers loaded at startup
    /// and any built-ins that registered themselves.
    pub fn from_registry(reg: &WorkerRegistry) -> Vec<Self> {
        reg.active_workers().iter().map(Self::from_handle).collect()
    }
}

/// The trait every LLM backend implements. Object-safe so the
/// pipeline can hold `Box<dyn LlmBackend>` chosen by config.
///
/// Returns:
/// - `Ok(Some(worker_id))` — the model picked a worker. The id is
///   *not* yet validated against the registry; that happens in the
///   cascade adapter, which also gates against unknown ids.
/// - `Ok(None)` — the model declined to pick (e.g. responded with
///   `none` in the default prompt template, or returned something
///   the backend's parser couldn't map to an id). The cascade
///   falls through to stage 3.
/// - `Err(_)` — backend transport failure (timeout, network, malformed
///   response). The adapter logs and falls through; never propagated
///   to the user — speed > precision (per spec).
pub trait LlmBackend: Send + Sync {
    fn classify(&self, prompt: &str, workers: &[WorkerInfo]) -> Result<Option<String>>;

    /// Short label for diagnostic output ("openai_compat", "oz", etc.).
    /// Used by `jarvis dispatcher status` and tracing log fields so
    /// users can tell which backend handled a given turn.
    fn name(&self) -> &str;
}

/// Build the default classifier prompt. Mirrors the contract documented
/// in the spec's "## How" section: list workers + hints, ask for the
/// id alone on the first line, and reserve `none` as the decline
/// sentinel.
///
/// Users who want a custom prompt drop a `dispatcher-prompt.txt` next
/// to their config (B-4 wires the override). The default is shipped
/// here so the system works zero-config.
pub fn default_classifier_prompt(transcript: &str, workers: &[WorkerInfo]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are Jarvis's intent router. Pick the single worker most \
         appropriate for the user's request. Reply with the worker id \
         alone on the first line — no punctuation, no explanation. If \
         no worker is a clearly better fit than the default, reply with \
         `none` on the first line.\n\n",
    );
    s.push_str("Available workers:\n");
    for w in workers {
        match &w.dispatch_hint {
            Some(hint) if !hint.is_empty() => {
                s.push_str(&format!("- {}: {}\n", w.id, hint));
            }
            _ => {
                s.push_str(&format!("- {}\n", w.id));
            }
        }
    }
    s.push_str("\nUser request:\n");
    s.push_str(transcript);
    s.push('\n');
    s
}

/// Parse a backend's raw reply into an optional worker id. The
/// classifier prompt asks for the id alone on the first line; this
/// helper enforces that contract, tolerating whitespace + the
/// `none` sentinel + chatty wrappers that some models can't help
/// adding ("The best worker is `time`.").
///
/// Returns `None` when the model declined (`none`, empty, "i don't
/// know"-style) or when no plausible id could be extracted. The
/// adapter further validates against the registry, so a non-`None`
/// here doesn't yet mean a real worker.
pub fn parse_worker_id(reply: &str) -> Option<String> {
    let first = reply.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return None;
    }
    let token = first
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
    if token.is_empty() {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "n/a" | "null" | "default") {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
pub(crate) mod testing {
    //! Test helpers — a deterministic mock backend for the trait
    //! contract and (eventually) for cascade-integration tests in
    //! B-4. Public to the crate under `cfg(test)` so the integration
    //! test in `src/dispatcher/mod.rs` and the future cascade test
    //! can both reach it.

    use super::*;
    use std::sync::Mutex;

    /// A backend that returns a fixed reply (or error) regardless of
    /// input. Records every invocation for assertions about what
    /// the cascade actually called.
    pub struct MockLlmBackend {
        pub reply: Result<Option<String>, String>,
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl MockLlmBackend {
        pub fn picking(worker_id: &str) -> Self {
            Self {
                reply: Ok(Some(worker_id.to_string())),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn declining() -> Self {
            Self {
                reply: Ok(None),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn failing(msg: &str) -> Self {
            Self {
                reply: Err(msg.to_string()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmBackend for MockLlmBackend {
        fn classify(&self, prompt: &str, workers: &[WorkerInfo]) -> Result<Option<String>> {
            self.calls.lock().unwrap().push((
                prompt.to_string(),
                workers.iter().map(|w| w.id.clone()).collect(),
            ));
            match &self.reply {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(anyhow::anyhow!(e.clone())),
            }
        }

        fn name(&self) -> &str {
            "mock"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MockLlmBackend;
    use super::*;
    use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

    struct FakeWorker {
        id: &'static str,
        hint: Option<&'static str>,
    }

    impl WorkerHandle for FakeWorker {
        fn id(&self) -> &str {
            self.id
        }
        fn dispatch_hint(&self) -> Option<&str> {
            self.hint
        }
        fn invoke(&self, _: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
            unreachable!("trait test should not invoke")
        }
    }

    /// `WorkerInfo::from_registry` snapshots every active worker
    /// with id + dispatch_hint preserved. The list is what we feed
    /// to the LLM each turn.
    #[test]
    fn worker_info_from_registry_snapshots_active_workers() {
        let mut reg = WorkerRegistry::default();
        reg.register_builtin(Arc::new(FakeWorker {
            id: "time",
            hint: Some("Use for clock queries."),
        }));
        reg.register_builtin(Arc::new(FakeWorker {
            id: "claude",
            hint: None,
        }));

        let infos = WorkerInfo::from_registry(&reg);
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id, "time");
        assert_eq!(
            infos[0].dispatch_hint.as_deref(),
            Some("Use for clock queries.")
        );
        assert_eq!(infos[1].id, "claude");
        assert_eq!(infos[1].dispatch_hint, None);
    }

    /// The default classifier prompt lists workers + hints and the
    /// user transcript, and instructs the model on the reply
    /// shape. Locking the contract because backend parsers depend
    /// on this exact behaviour ("id alone on first line", `none`
    /// sentinel).
    #[test]
    fn default_prompt_includes_workers_and_transcript() {
        let workers = vec![
            WorkerInfo {
                id: "time".to_string(),
                dispatch_hint: Some("Clock queries.".to_string()),
            },
            WorkerInfo {
                id: "claude".to_string(),
                dispatch_hint: None,
            },
        ];
        let p = default_classifier_prompt("qué hora es en Tokio", &workers);
        assert!(p.contains("- time: Clock queries."));
        assert!(p.contains("- claude"));
        assert!(
            !p.contains("- claude:"),
            "no-hint workers should not have a colon"
        );
        assert!(p.contains("qué hora es en Tokio"));
        assert!(
            p.contains("`none`") || p.contains("none"),
            "prompt must mention the decline sentinel"
        );
        assert!(
            p.to_lowercase().contains("first line"),
            "prompt must tell the model where to put the id"
        );
    }

    /// `parse_worker_id` accepts the canonical "id alone on first
    /// line" reply and tolerates chatty wrappers + trailing
    /// punctuation. Matches what real models tend to produce even
    /// when told not to.
    #[test]
    fn parse_worker_id_handles_canonical_and_chatty_replies() {
        assert_eq!(parse_worker_id("time"), Some("time".to_string()));
        assert_eq!(parse_worker_id("  time\n"), Some("time".to_string()));
        assert_eq!(
            parse_worker_id("time\nmore stuff"),
            Some("time".to_string())
        );
        assert_eq!(parse_worker_id("`time`"), Some("time".to_string()));
        assert_eq!(parse_worker_id("time."), Some("time".to_string()));
        assert_eq!(parse_worker_id("task-list"), Some("task-list".to_string()));
        assert_eq!(
            parse_worker_id("session_reset"),
            Some("session_reset".to_string())
        );
    }

    /// The decline sentinels and empty replies map to `None` so
    /// the cascade falls through to stage 3 instead of routing to
    /// a phantom worker.
    #[test]
    fn parse_worker_id_treats_decline_sentinels_as_none() {
        assert_eq!(parse_worker_id(""), None);
        assert_eq!(parse_worker_id("\n\n"), None);
        assert_eq!(parse_worker_id("none"), None);
        assert_eq!(parse_worker_id("NONE"), None);
        assert_eq!(parse_worker_id("n/a"), None);
        assert_eq!(parse_worker_id("null"), None);
        assert_eq!(parse_worker_id("default"), None);
        assert_eq!(parse_worker_id("..."), None);
    }

    /// Trait smoke through the mock: each call records its prompt +
    /// the worker ids the backend was shown. Confirms object-safety
    /// (we store as `Box<dyn LlmBackend>`).
    #[test]
    fn mock_backend_records_calls_and_returns_configured_reply() {
        let backend: Box<dyn LlmBackend> = Box::new(MockLlmBackend::picking("time"));
        let workers = vec![WorkerInfo {
            id: "time".to_string(),
            dispatch_hint: None,
        }];
        let out = backend.classify("qué hora es", &workers).unwrap();
        assert_eq!(out.as_deref(), Some("time"));
        assert_eq!(backend.name(), "mock");
    }

    /// A backend returning `Err` is allowed; the cascade adapter
    /// (B-4) is responsible for swallowing it. The trait itself
    /// just propagates.
    #[test]
    fn mock_backend_can_signal_failure() {
        let backend: Box<dyn LlmBackend> = Box::new(MockLlmBackend::failing("classifier offline"));
        let err = backend.classify("anything", &[]).expect_err("should fail");
        assert!(format!("{err:#}").contains("classifier offline"));
    }

    /// Declining backend returns `Ok(None)` — the documented "no
    /// clear pick" signal. Distinct from `Err` so the adapter can
    /// log differently for genuine failures vs intentional
    /// declines.
    #[test]
    fn mock_backend_can_decline() {
        let backend: Box<dyn LlmBackend> = Box::new(MockLlmBackend::declining());
        assert!(backend.classify("anything", &[]).unwrap().is_none());
    }
}
