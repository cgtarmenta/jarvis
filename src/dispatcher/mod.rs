//! Dispatcher cascade — decides which worker handles each turn.
//!
//! Spec 0010 (orchestrator A) introduces a three-stage cascade:
//!
//! 1. **Stage 1** ([`BuiltinIntentDispatcher`], A-2): deterministic
//!    intent matching against built-in handlers (time / calc / spec /
//!    session-reset).
//! 2. **Stage 2** (LLM dispatcher, hija B): a pluggable LLM-based
//!    classifier; optional, configured under `[listener.fallback]`.
//! 3. **Stage 3** ([`DefaultWorkerDispatcher`], here): always returns
//!    the configured default worker (`cfg.agent.name`) with the
//!    prompt verbatim. The last-resort match.
//!
//! Today only stages 1 and 3 are wired; stage 2 is a slot reserved
//! for hija B.

pub mod cascade;
pub mod default;
pub mod intent;

use anyhow::Result;

use crate::session::Session;
use crate::workers::WorkerRegistry;

pub use cascade::CascadeDispatcher;
pub use default::DefaultWorkerDispatcher;
pub use intent::{BuiltinIntentDispatcher, IntentMatcher};

/// The dispatcher's decision for a single turn.
///
/// `resolved_prompt` is whatever text the chosen worker should
/// actually receive — usually the user's transcript verbatim, but a
/// dispatcher *may* rewrite it (e.g. resolve "y en Tokio?" into
/// "what time is it in Tokio?" using prior conversation context).
/// `session_id` is the worker's pre-invocation session id, lifted
/// from `session.active_workers[worker_id]` when the worker is
/// stateful and has a known prior session on this thread.
#[derive(Debug, Clone)]
pub struct DispatchDecision {
    pub worker_id: String,
    pub resolved_prompt: String,
    pub session_id: Option<String>,
}

/// Object-safe trait every dispatcher stage implements.
///
/// `Ok(Some(decision))` claims the turn; `Ok(None)` declines and the
/// cascade moves to the next stage. `Err(...)` propagates as a turn
/// failure — most stages will never return `Err` because their job
/// is to *decide*, not to invoke.
pub trait Dispatcher: Send + Sync {
    fn dispatch(
        &self,
        prompt: &str,
        session: &Session,
        registry: &WorkerRegistry,
    ) -> Result<Option<DispatchDecision>>;
}
