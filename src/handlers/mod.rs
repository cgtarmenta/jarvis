//! Built-in handlers — Rust-coded `IntentMatcher` + `WorkerHandle`
//! pairs that ship in-process and self-register into the
//! `WorkerRegistry` at daemon startup.
//!
//! Spec 0010 (orchestrator A) introduces this module as the home
//! for stage-1 deterministic intents. Each handler implements *both*
//! traits because the dispatcher's recognise step and the
//! pipeline's invoke step are two halves of the same conceptual
//! piece. Handlers stay tiny (~50-100 LoC) so adding a new one is
//! a copy-paste pattern rather than a framework adventure.

pub mod session_reset;
pub mod spec;

pub use session_reset::SessionResetHandler;
pub use spec::SpecHandler;

use std::sync::Arc;

use crate::config::JarvisConfig;
use crate::dispatcher::IntentMatcher;
use crate::workers::{WorkerHandle, WorkerRegistry};

/// Register every built-in handler with the worker registry *and*
/// return the matchers list for the `BuiltinIntentDispatcher` to
/// iterate. Each handler is constructed twice — once as a
/// `WorkerHandle` for the registry and once as an `IntentMatcher`
/// for the dispatcher — because trait-object coercion in Rust
/// can't share a single `Arc<T>` between two unrelated trait
/// objects. The handlers are stateless or hold a small cloneable
/// config (e.g. reset phrases), so dual construction is cheap.
///
/// Built-in order matters: the dispatcher consults matchers in
/// the order they appear in the returned vector. Put more-specific
/// matchers earlier; the spec handler beats the session-reset
/// handler because spec phrases are longer and unambiguous, while
/// reset phrases are short (`olvida`, `reset`) and could
/// theoretically overlap with substrings of real user requests if
/// we ever loosen the equality check.
pub fn register_builtins(
    registry: &mut WorkerRegistry,
    cfg: &JarvisConfig,
) -> Vec<Arc<dyn IntentMatcher>> {
    let mut matchers: Vec<Arc<dyn IntentMatcher>> = Vec::new();

    // 1. Spec management — longer phrases, more specific. Must come
    //    before reset so "borra el spec" doesn't accidentally trip
    //    the reset path.
    let spec_worker: Arc<dyn WorkerHandle> = Arc::new(SpecHandler);
    registry.register_builtin(spec_worker);
    matchers.push(Arc::new(SpecHandler));

    // 2. Session reset — the user's `reset_phrases` from config,
    //    matched as exact normalised equality. Short, terminal.
    let reset_worker: Arc<dyn WorkerHandle> =
        Arc::new(SessionResetHandler::new(cfg.session.reset_phrases.clone()));
    registry.register_builtin(reset_worker);
    matchers.push(Arc::new(SessionResetHandler::new(
        cfg.session.reset_phrases.clone(),
    )));

    matchers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: `register_builtins` populates the registry and the
    /// matchers list in lockstep. Each handler appears as both an
    /// active worker (for invoke) and an intent matcher (for
    /// dispatch). Spec handler comes before session-reset.
    #[test]
    fn register_builtins_dual_registration() {
        let cfg = JarvisConfig::default();
        let mut registry = WorkerRegistry::default();
        let matchers = register_builtins(&mut registry, &cfg);

        // Registry has both worker entries.
        assert!(registry.get("spec").is_some(), "spec worker registered");
        assert!(
            registry.get("session-reset").is_some(),
            "session-reset worker registered"
        );

        // Matchers list is ordered spec → session-reset.
        assert_eq!(matchers.len(), 2);
        assert_eq!(matchers[0].worker_id(), "spec");
        assert_eq!(matchers[1].worker_id(), "session-reset");
    }
}
