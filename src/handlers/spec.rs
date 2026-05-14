//! Built-in handler for voice-driven spec management.
//!
//! Thin wrapper over the existing `crate::specs::{recognize,execute}`
//! functions shipped by spec 0006 (voice-driven spec management).
//! The handler implements both traits required by the dispatcher
//! cascade: `IntentMatcher` (for recognition) and `WorkerHandle`
//! (for execution). The legacy inline check in `pipeline::run_turn`
//! stays in place until A-4 wires the dispatcher up — until then,
//! this handler is registered but unused.

use anyhow::{Result, anyhow};

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

pub struct SpecHandler;

impl IntentMatcher for SpecHandler {
    fn worker_id(&self) -> &str {
        "spec"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        // Delegate to the existing prefix-matching recogniser. We
        // re-parse inside `invoke` because the dispatcher → worker
        // contract is "pass the resolved prompt"; a sub-millisecond
        // string scan twice is cheaper than threading the parsed
        // `Intent` through a separate field on `DispatchDecision`.
        crate::specs::recognize(prompt).map(|_| prompt.to_string())
    }
}

impl WorkerHandle for SpecHandler {
    fn id(&self) -> &str {
        "spec"
    }

    fn description(&self) -> Option<&str> {
        Some("Voice-driven spec management (open / list / show / promote / ship / reject).")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Best when the user wants to manage specs in this repo: \
             open a spec, list specs, show / promote / ship / reject a \
             specific spec.",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let intent = crate::specs::recognize(ctx.prompt).ok_or_else(|| {
            anyhow!(
                "spec handler invoked but no spec intent matched in prompt: {:?}",
                ctx.prompt
            )
        })?;
        let text = crate::specs::execute(intent);
        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler claims prompts that the existing
    /// `specs::recognize` recognises. Verifies the bridge works
    /// end-to-end against the real recogniser, not a stub.
    #[test]
    fn recognises_a_spec_open_phrase() {
        let h = SpecHandler;
        let session = Session::new();
        let resolved = h
            .recognize("abre un spec para streaming TTS", &session)
            .expect("recognised");
        // Resolved prompt is the verbatim input — the handler
        // re-parses on invoke rather than transforming here.
        assert_eq!(resolved, "abre un spec para streaming TTS");
    }

    /// Non-spec prompts return `None`, letting the cascade move on.
    #[test]
    fn declines_non_spec_prompts() {
        let h = SpecHandler;
        let session = Session::new();
        assert!(h.recognize("hola", &session).is_none());
        assert!(h.recognize("¿qué hora es?", &session).is_none());
    }

    /// The handler exposes the same id on both the IntentMatcher
    /// and WorkerHandle sides. The dispatcher relies on this
    /// invariant to route turns from match → registry lookup.
    #[test]
    fn ids_match_across_traits() {
        let h = SpecHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}
