//! `DefaultWorkerDispatcher` — the last cascade stage.
//!
//! Always returns a decision pointing at the configured default
//! worker (`cfg.agent.name`, today usually `"claude"`). The
//! `resolved_prompt` is the user's transcript verbatim; for
//! stateful workers the dispatcher lifts the worker's prior
//! session id out of `session.active_workers`.

use anyhow::Result;

use super::{DispatchDecision, Dispatcher};
use crate::session::Session;
use crate::workers::WorkerRegistry;

pub struct DefaultWorkerDispatcher {
    worker_id: String,
}

impl DefaultWorkerDispatcher {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }
}

impl Dispatcher for DefaultWorkerDispatcher {
    fn dispatch(
        &self,
        prompt: &str,
        session: &Session,
        _registry: &WorkerRegistry,
    ) -> Result<Option<DispatchDecision>> {
        // Resume from any prior session id for this worker on this
        // thread. `session.active_worker_session` returns
        // `Some(Some(uuid))` for a stateful worker we've talked to,
        // `Some(None)` for a stateless one (uuid stays None), and
        // `None` when we've never invoked this worker — also uuid
        // stays None for that case (the worker will start fresh).
        let session_id = session
            .active_worker_session(&self.worker_id)
            .and_then(|opt| opt.clone());

        Ok(Some(DispatchDecision {
            worker_id: self.worker_id.clone(),
            resolved_prompt: prompt.to_string(),
            session_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Role, Session};
    use crate::workers::WorkerRegistry;

    /// The default dispatcher always claims the turn — every input
    /// gets routed to the configured worker, regardless of content.
    /// This is what makes it the cascade's catch-all.
    #[test]
    fn default_dispatcher_always_returns_some() {
        let d = DefaultWorkerDispatcher::new("claude");
        let session = Session::new();
        let registry = WorkerRegistry::default();
        let decision = d
            .dispatch("anything at all", &session, &registry)
            .unwrap()
            .expect("default always returns Some");
        assert_eq!(decision.worker_id, "claude");
        assert_eq!(decision.resolved_prompt, "anything at all");
        assert!(decision.session_id.is_none(), "no prior session yet");
    }

    /// When the session has a recorded `active_workers[worker_id]`,
    /// that session id flows into the dispatch decision so the
    /// worker resumes its prior context.
    #[test]
    fn default_dispatcher_carries_session_id_from_active_workers() {
        let d = DefaultWorkerDispatcher::new("claude");
        let mut session = Session::new();
        session.set_active_worker_session("claude", Some("uuid-from-prior-turn".into()));
        // Throw in a turn so we know the session isn't pristine.
        session.add_turn_for_worker(
            Role::User,
            "earlier".into(),
            "claude".into(),
            Some("uuid-from-prior-turn".into()),
        );

        let registry = WorkerRegistry::default();
        let decision = d
            .dispatch("hola", &session, &registry)
            .unwrap()
            .expect("default returns Some");
        assert_eq!(decision.session_id.as_deref(), Some("uuid-from-prior-turn"));
    }

    /// A stateless worker recorded as `Some(None)` in
    /// active_workers (i.e. invoked but never produced a session
    /// id) yields a `None` session_id in the decision, not the
    /// `Some(None)` outer wrapper.
    #[test]
    fn default_dispatcher_unwraps_stateless_marker() {
        let d = DefaultWorkerDispatcher::new("time");
        let mut session = Session::new();
        session.set_active_worker_session("time", None);
        let registry = WorkerRegistry::default();
        let decision = d.dispatch("hola", &session, &registry).unwrap().unwrap();
        assert!(decision.session_id.is_none());
    }

    /// `worker_id` accessor surfaces the configured target — used
    /// by future diagnostics that need to introspect the cascade.
    #[test]
    fn worker_id_accessor() {
        let d = DefaultWorkerDispatcher::new("custom");
        assert_eq!(d.worker_id(), "custom");
    }
}
