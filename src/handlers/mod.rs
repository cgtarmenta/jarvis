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

pub mod app_launcher;
pub mod calc;
pub mod date_today;
pub mod session_reset;
pub mod spec;
pub mod task_cancel;
pub mod task_clean;
pub mod task_list;
pub mod task_show;
pub mod time_of_day;

pub use app_launcher::AppLauncherHandler;
pub use calc::CalcHandler;
pub use date_today::DateTodayHandler;
pub use session_reset::SessionResetHandler;
pub use spec::SpecHandler;
pub use task_cancel::TaskCancelHandler;
pub use task_clean::TaskCleanHandler;
pub use task_list::TaskListHandler;
pub use task_show::TaskShowHandler;
pub use time_of_day::TimeOfDayHandler;

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

    // 2. Time — "qué hora es" / "what time is it", optional city.
    let time_worker: Arc<dyn WorkerHandle> = Arc::new(TimeOfDayHandler);
    registry.register_builtin(time_worker);
    matchers.push(Arc::new(TimeOfDayHandler));

    // 3. Date — "qué fecha", "what date is it".
    let date_worker: Arc<dyn WorkerHandle> = Arc::new(DateTodayHandler);
    registry.register_builtin(date_worker);
    matchers.push(Arc::new(DateTodayHandler));

    // 4. Calc — arithmetic with spoken-word numbers and operators.
    //    Triggers like "cuánto es" / "calculate" require the tail
    //    to look numeric; non-arithmetic prompts with those trigger
    //    words still fall through.
    let calc_worker: Arc<dyn WorkerHandle> = Arc::new(CalcHandler);
    registry.register_builtin(calc_worker);
    matchers.push(Arc::new(CalcHandler));

    // 5. App launcher (spec 0015) — "abre Firefox", "launch Spotify".
    //    Position AFTER spec is load-bearing: spec's triggers
    //    include `"abre un spec para "`, which shares the
    //    `"abre "` prefix with the app launcher. Spec runs first
    //    so spec-management phrases route correctly; only "abre
    //    <app>" phrases that don't match spec land here.
    //    User-defined aliases from `[apps.aliases]` override the
    //    built-in alias table.
    let app_launcher_worker: Arc<dyn WorkerHandle> = Arc::new(
        AppLauncherHandler::with_user_aliases(cfg.apps.aliases.clone()),
    );
    registry.register_builtin(app_launcher_worker);
    matchers.push(Arc::new(AppLauncherHandler::with_user_aliases(
        cfg.apps.aliases.clone(),
    )));

    // 6. Task list (spec 0012 / E2) — "qué tareas tengo", etc.
    //    Position before session-reset because the reset phrase
    //    list is short and shouldn't trip on a substring of a
    //    task-list utterance.
    let task_list_worker: Arc<dyn WorkerHandle> = Arc::new(TaskListHandler);
    registry.register_builtin(task_list_worker);
    matchers.push(Arc::new(TaskListHandler));

    // 7. Task show (spec 0012 / E2) — "muéstrame el resultado de X".
    let task_show_worker: Arc<dyn WorkerHandle> = Arc::new(TaskShowHandler);
    registry.register_builtin(task_show_worker);
    matchers.push(Arc::new(TaskShowHandler));

    // 8. Task cancel (spec 0012 / E2) — "cancela esa tarea".
    let task_cancel_worker: Arc<dyn WorkerHandle> = Arc::new(TaskCancelHandler);
    registry.register_builtin(task_cancel_worker);
    matchers.push(Arc::new(TaskCancelHandler));

    // 9. Task clean (spec 0012 / E2) — "limpia las viejas".
    let task_clean_worker: Arc<dyn WorkerHandle> = Arc::new(TaskCleanHandler);
    registry.register_builtin(task_clean_worker);
    matchers.push(Arc::new(TaskCleanHandler));

    // 10. Session reset — short, terminal. Last because its phrase
    //    list (`olvida`, `reset`) is so short it could match
    //    substrings of the others if we ever relax equality.
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

    /// Spec 0012 / E2 smoke: the four task-voice intents route
    /// to their handlers when the cascade is assembled the
    /// same way the pipeline does. Locking the routing
    /// behaviour so a future trigger-phrase tweak doesn't
    /// silently steal one of these prompts.
    #[test]
    fn task_voice_intents_route_through_cascade() {
        use crate::dispatcher::{
            BuiltinIntentDispatcher, CascadeDispatcher, DefaultWorkerDispatcher, Dispatcher,
        };
        use crate::session::Session;

        let cfg = JarvisConfig::default();
        let mut registry = WorkerRegistry::default();
        let matchers = register_builtins(&mut registry, &cfg);
        let cascade = CascadeDispatcher::new()
            .push(Box::new(BuiltinIntentDispatcher::from_matchers(matchers)))
            .push(Box::new(DefaultWorkerDispatcher::new("claude")));
        let session = Session::new();

        for (prompt, expected) in [
            ("¿qué tareas tengo?", "task-list"),
            ("muéstrame el resultado del análisis", "task-show"),
            ("qué dijo gemini", "task-show"),
            ("cancela esa tarea", "task-cancel"),
            ("para la tarea de claude", "task-cancel"),
            ("limpia las tareas viejas", "task-clean"),
            ("purge tasks", "task-clean"),
        ] {
            let d = cascade
                .dispatch(prompt, &session, &registry)
                .unwrap()
                .unwrap();
            assert_eq!(
                d.worker_id, expected,
                "prompt {prompt:?} should route to {expected:?}, got {:?}",
                d.worker_id
            );
        }
    }

    /// End-to-end smoke for the spec 0010 cascade: a few
    /// representative prompts get routed to the right worker
    /// through the same composition the pipeline assembles. The
    /// default-worker stage at the end catches anything the
    /// built-in matchers decline.
    #[test]
    fn full_cascade_routes_prompts_to_expected_workers() {
        use crate::dispatcher::{
            BuiltinIntentDispatcher, CascadeDispatcher, DefaultWorkerDispatcher, Dispatcher,
        };
        use crate::session::Session;

        let cfg = JarvisConfig::default();
        let mut registry = WorkerRegistry::default();
        let matchers = register_builtins(&mut registry, &cfg);
        let cascade = CascadeDispatcher::new()
            .push(Box::new(BuiltinIntentDispatcher::from_matchers(matchers)))
            .push(Box::new(DefaultWorkerDispatcher::new("claude")));
        let session = Session::new();

        // Time query → time handler (stage 1).
        let d = cascade
            .dispatch("¿qué hora es?", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(d.worker_id, "time", "time query → time handler");

        // Date query → date handler.
        let d = cascade
            .dispatch("qué día es hoy", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(d.worker_id, "date", "date query → date handler");

        // Arithmetic → calc handler.
        let d = cascade
            .dispatch("cuánto es dos más tres", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(d.worker_id, "calc", "arithmetic → calc handler");

        // Spec phrase → spec handler.
        let d = cascade
            .dispatch("abre un spec para streaming TTS", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(d.worker_id, "spec", "spec phrase → spec handler");

        // Reset phrase → session-reset handler.
        let d = cascade
            .dispatch("olvida todo", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(
            d.worker_id, "session-reset",
            "reset phrase → session-reset handler"
        );

        // Unrelated prompt → falls through to the default (claude).
        let d = cascade
            .dispatch("explícame los protocolos de gossip", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(
            d.worker_id, "claude",
            "unmatched prompt → default worker (claude)"
        );
    }

    /// Smoke: `register_builtins` populates the registry and the
    /// matchers list in lockstep. Each handler appears as both an
    /// active worker (for invoke) and an intent matcher (for
    /// dispatch). Order is spec → time → date → calc → reset.
    #[test]
    fn register_builtins_dual_registration() {
        let cfg = JarvisConfig::default();
        let mut registry = WorkerRegistry::default();
        let matchers = register_builtins(&mut registry, &cfg);

        // Registry has all ten worker entries (spec 0015 added
        // app-launcher between calc and task-list).
        for id in [
            "spec",
            "time",
            "date",
            "calc",
            "app-launcher",
            "task-list",
            "task-show",
            "task-cancel",
            "task-clean",
            "session-reset",
        ] {
            assert!(
                registry.get(id).is_some(),
                "{id} worker should be registered"
            );
        }

        // Matchers list mirrors registration order.
        assert_eq!(matchers.len(), 10);
        let ids: Vec<&str> = matchers.iter().map(|m| m.worker_id()).collect();
        assert_eq!(
            ids,
            vec![
                "spec",
                "time",
                "date",
                "calc",
                "app-launcher",
                "task-list",
                "task-show",
                "task-cancel",
                "task-clean",
                "session-reset",
            ]
        );
    }

    /// Spec 0015 — app-launcher routes "abre <app>" to its
    /// handler, and the spec → app-launcher ordering means
    /// "abre un spec para X" still routes to spec (not
    /// app-launcher). Locking this so a future re-ordering of
    /// register_builtins doesn't silently break either path.
    #[test]
    fn app_launcher_and_spec_share_abre_prefix_correctly() {
        use crate::dispatcher::{
            BuiltinIntentDispatcher, CascadeDispatcher, DefaultWorkerDispatcher, Dispatcher,
        };
        use crate::session::Session;

        let cfg = JarvisConfig::default();
        let mut registry = WorkerRegistry::default();
        let matchers = register_builtins(&mut registry, &cfg);
        let cascade = CascadeDispatcher::new()
            .push(Box::new(BuiltinIntentDispatcher::from_matchers(matchers)))
            .push(Box::new(DefaultWorkerDispatcher::new("claude")));
        let session = Session::new();

        // "abre Firefox" → app-launcher (no spec match).
        let d = cascade
            .dispatch("abre Firefox", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(
            d.worker_id, "app-launcher",
            "abre <app> should route to app-launcher"
        );

        // "abre un spec para streaming TTS" → spec (longer
        // trigger wins because spec is registered first).
        let d = cascade
            .dispatch("abre un spec para streaming TTS", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(
            d.worker_id, "spec",
            "abre un spec para X must still route to spec"
        );

        // "launch Spotify" → app-launcher.
        let d = cascade
            .dispatch("launch Spotify", &session, &registry)
            .unwrap()
            .unwrap();
        assert_eq!(d.worker_id, "app-launcher");
    }
}
