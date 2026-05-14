//! Stage 1 of the cascade — deterministic intent matching.
//!
//! `BuiltinIntentDispatcher` walks an ordered list of `IntentMatcher`s
//! and routes the prompt to whichever handler claims it first. The
//! matchers themselves live in `src/handlers/` (see hija A spec) and
//! also implement `WorkerHandle` so they're invokable by the same
//! pipeline that runs manifest-loaded workers.

use std::sync::Arc;

use anyhow::Result;

use super::{DispatchDecision, Dispatcher};
use crate::session::Session;
use crate::workers::WorkerRegistry;

/// Recognition half of a built-in handler. Pairs with `WorkerHandle`
/// (the execution half) on the same struct; see e.g.
/// `handlers::SpecHandler` for a worked example.
pub trait IntentMatcher: Send + Sync {
    /// The worker id this matcher dispatches to when it claims a turn.
    /// Must match the `WorkerHandle::id()` of the same handler so the
    /// pipeline can look it up in the registry after the dispatch.
    fn worker_id(&self) -> &str;

    /// Inspect `prompt` (and optionally `session`, for context-aware
    /// follow-up resolution); return `Some(resolved_prompt)` to claim
    /// the turn or `None` to defer. The `resolved_prompt` is what the
    /// worker will actually receive in its `WorkerInvocation`;
    /// matchers may rewrite the user's transcript before passing it
    /// along (e.g. resolve "y en Tokio?" → "what time is it in Tokio?").
    fn recognize(&self, prompt: &str, session: &Session) -> Option<String>;
}

/// `Dispatcher` implementation that consults a fixed list of
/// `IntentMatcher`s in order. Used as the cascade's stage 1.
pub struct BuiltinIntentDispatcher {
    matchers: Vec<Arc<dyn IntentMatcher>>,
}

impl BuiltinIntentDispatcher {
    pub fn new() -> Self {
        Self {
            matchers: Vec::new(),
        }
    }

    /// Builder: append a matcher to the end of the ordered list.
    /// Order matters — the first matcher to claim a prompt wins, so
    /// register more specific matchers before more general ones.
    pub fn push(mut self, matcher: Arc<dyn IntentMatcher>) -> Self {
        self.matchers.push(matcher);
        self
    }

    pub fn matcher_count(&self) -> usize {
        self.matchers.len()
    }
}

impl Default for BuiltinIntentDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher for BuiltinIntentDispatcher {
    fn dispatch(
        &self,
        prompt: &str,
        session: &Session,
        _registry: &WorkerRegistry,
    ) -> Result<Option<DispatchDecision>> {
        for m in &self.matchers {
            if let Some(resolved) = m.recognize(prompt, session) {
                let worker_id = m.worker_id().to_string();
                // Lift the worker's prior session id from spec D's
                // map. Stateless handlers will have `None` here and
                // the worker's `invoke` will ignore the field.
                let session_id = session
                    .active_worker_session(&worker_id)
                    .and_then(|opt| opt.clone());
                return Ok(Some(DispatchDecision {
                    worker_id,
                    resolved_prompt: resolved,
                    session_id,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::workers::WorkerRegistry;

    struct StubMatcher {
        id: &'static str,
        prefix: &'static str,
    }

    impl IntentMatcher for StubMatcher {
        fn worker_id(&self) -> &str {
            self.id
        }
        fn recognize(&self, prompt: &str, _: &Session) -> Option<String> {
            prompt.strip_prefix(self.prefix).map(|s| s.trim().to_string())
        }
    }

    /// First-match wins: registration order is dispatch order, and
    /// once a matcher claims the turn the rest don't even run.
    #[test]
    fn first_matcher_to_claim_wins() {
        let dispatcher = BuiltinIntentDispatcher::new()
            .push(Arc::new(StubMatcher {
                id: "alpha",
                prefix: "alpha ",
            }))
            .push(Arc::new(StubMatcher {
                id: "beta",
                prefix: "beta ",
            }));

        let session = Session::new();
        let registry = WorkerRegistry::default();
        let decision = dispatcher
            .dispatch("alpha hello world", &session, &registry)
            .unwrap()
            .expect("alpha claimed");
        assert_eq!(decision.worker_id, "alpha");
        // The matcher rewrote the prompt by stripping its prefix.
        assert_eq!(decision.resolved_prompt, "hello world");
    }

    /// All matchers declining → cascade-style `None`, lets later
    /// stages (e.g. the default-worker dispatcher) run.
    #[test]
    fn no_matches_returns_none() {
        let dispatcher = BuiltinIntentDispatcher::new()
            .push(Arc::new(StubMatcher {
                id: "alpha",
                prefix: "alpha ",
            }))
            .push(Arc::new(StubMatcher {
                id: "beta",
                prefix: "beta ",
            }));

        let session = Session::new();
        let registry = WorkerRegistry::default();
        assert!(
            dispatcher
                .dispatch("something else entirely", &session, &registry)
                .unwrap()
                .is_none()
        );
    }

    /// Session lookups feed through — when active_workers has a
    /// recorded id for the matcher's worker, it appears in the
    /// dispatch decision.
    #[test]
    fn session_id_flows_through_when_recorded() {
        let dispatcher =
            BuiltinIntentDispatcher::new().push(Arc::new(StubMatcher {
                id: "stateful",
                prefix: "go ",
            }));

        let mut session = Session::new();
        session.set_active_worker_session("stateful", Some("uuid-prior".into()));
        let registry = WorkerRegistry::default();
        let decision = dispatcher
            .dispatch("go please", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(decision.session_id.as_deref(), Some("uuid-prior"));
    }

    /// `matcher_count` exposes the registered count — used by
    /// future diagnostic surfaces.
    #[test]
    fn matcher_count_reflects_pushes() {
        let d = BuiltinIntentDispatcher::new()
            .push(Arc::new(StubMatcher {
                id: "a",
                prefix: "a ",
            }))
            .push(Arc::new(StubMatcher {
                id: "b",
                prefix: "b ",
            }))
            .push(Arc::new(StubMatcher {
                id: "c",
                prefix: "c ",
            }));
        assert_eq!(d.matcher_count(), 3);
    }
}
