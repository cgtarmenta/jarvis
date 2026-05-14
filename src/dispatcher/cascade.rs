//! `CascadeDispatcher` — composes ordered dispatcher stages.

use anyhow::Result;

use super::{DispatchDecision, Dispatcher};
use crate::session::Session;
use crate::workers::WorkerRegistry;

/// Tries each stage in order. The first stage to return `Some`
/// claims the turn; if every stage returns `None` the cascade
/// itself returns `None`. The pipeline composes a cascade ending
/// in [`DefaultWorkerDispatcher`] so the last stage always claims
/// a turn — `None` from a fully-configured cascade is therefore a
/// programmer error and should surface as a clear log line at the
/// call site.
pub struct CascadeDispatcher {
    stages: Vec<Box<dyn Dispatcher>>,
}

impl CascadeDispatcher {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Builder-style: append a stage to the end. Stages run in
    /// insertion order, so the call sequence is the cascade
    /// sequence.
    pub fn push(mut self, stage: Box<dyn Dispatcher>) -> Self {
        self.stages.push(stage);
        self
    }

    /// Count of stages registered — exposed for diagnostic output
    /// (e.g. a future `jarvis dispatcher status` command).
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

impl Default for CascadeDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher for CascadeDispatcher {
    fn dispatch(
        &self,
        prompt: &str,
        session: &Session,
        registry: &WorkerRegistry,
    ) -> Result<Option<DispatchDecision>> {
        for stage in &self.stages {
            if let Some(decision) = stage.dispatch(prompt, session, registry)? {
                return Ok(Some(decision));
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

    /// A stub dispatcher that either returns a fixed decision or
    /// declines, used to exercise cascade ordering without setting
    /// up real handlers.
    struct StubDispatcher {
        worker_id: Option<&'static str>,
    }

    impl Dispatcher for StubDispatcher {
        fn dispatch(
            &self,
            prompt: &str,
            _: &Session,
            _: &WorkerRegistry,
        ) -> Result<Option<DispatchDecision>> {
            Ok(self.worker_id.map(|id| DispatchDecision {
                worker_id: id.to_string(),
                resolved_prompt: prompt.to_string(),
                session_id: None,
            }))
        }
    }

    /// The first stage to return `Some` wins. Later stages — even
    /// ones that would also have matched — never run.
    #[test]
    fn cascade_returns_first_match() {
        let cascade = CascadeDispatcher::new()
            .push(Box::new(StubDispatcher {
                worker_id: Some("first"),
            }))
            .push(Box::new(StubDispatcher {
                worker_id: Some("second"),
            }));

        let session = Session::new();
        let registry = WorkerRegistry::default();
        let decision = cascade
            .dispatch("hi", &session, &registry)
            .unwrap()
            .expect("cascade returns first match");
        assert_eq!(decision.worker_id, "first");
    }

    /// A stage returning `None` is skipped; the next non-`None`
    /// stage claims the turn.
    #[test]
    fn cascade_skips_none_stages() {
        let cascade = CascadeDispatcher::new()
            .push(Box::new(StubDispatcher { worker_id: None }))
            .push(Box::new(StubDispatcher {
                worker_id: Some("matched"),
            }));

        let session = Session::new();
        let registry = WorkerRegistry::default();
        let decision = cascade
            .dispatch("hi", &session, &registry)
            .unwrap()
            .expect("non-None stage wins");
        assert_eq!(decision.worker_id, "matched");
    }

    /// All stages returning `None` produces `Ok(None)` — the
    /// pipeline is expected to compose a cascade that includes
    /// the default-worker stage to guarantee a match.
    #[test]
    fn cascade_returns_none_when_all_stages_decline() {
        let cascade = CascadeDispatcher::new()
            .push(Box::new(StubDispatcher { worker_id: None }))
            .push(Box::new(StubDispatcher { worker_id: None }));

        let session = Session::new();
        let registry = WorkerRegistry::default();
        assert!(
            cascade
                .dispatch("hi", &session, &registry)
                .unwrap()
                .is_none()
        );
    }

    /// `stage_count` exposes the number of registered stages — used
    /// by future diagnostic surfaces.
    #[test]
    fn stage_count_reflects_pushes() {
        let cascade = CascadeDispatcher::new()
            .push(Box::new(StubDispatcher { worker_id: None }))
            .push(Box::new(StubDispatcher { worker_id: None }));
        assert_eq!(cascade.stage_count(), 2);
    }
}
