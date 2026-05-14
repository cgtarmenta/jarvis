//! One-shot record → STT → agent → TTS turn.
//!
//! Both `jarvis listen` (hotkey-triggered) and the wake-word daemon call
//! [`run_once`]. Keeping the orchestration in one place ensures both entry
//! points go through the same code path; no chance of one drifting from the
//! other as the project grows.

use std::fs;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::agents;
use crate::config::{self as cfg_mod, JarvisConfig, RecordConfig};
use crate::dispatcher::{
    BuiltinIntentDispatcher, CascadeDispatcher, DefaultWorkerDispatcher, Dispatcher, IntentMatcher,
    LlmDispatcher, build_llm_stage,
};
use crate::handlers;
use crate::recorder;
use crate::session::{self, Role};
use crate::stt;
use crate::tts;
use crate::workers::{WorkerInvocation, WorkerRegistry, WorkerResponse};

/// Per-turn overrides over what is normally read from `JarvisConfig`.
///
/// The follow-up listening window (spec 0007) needs three adjustments
/// from the standard turn: skip the audible cue (we are already
/// mid-conversation), allow a per-turn record override, and — most
/// importantly — start with a speech-onset gate so we don't sit in a
/// hard-deadline recorder cutting the user off mid-sentence.
#[derive(Debug, Clone, Default)]
pub struct TurnOptions {
    /// Override the [`RecordConfig`] for this turn only. Currently
    /// unused at the call sites we ship with — kept as a hook for
    /// future overrides like a quieter cue or a noisier-mic profile.
    pub record_override: Option<RecordConfig>,
    /// Whether to play the "I'm listening" cue at the start of the turn.
    /// Follow-up turns pass `false`; wake-triggered and hotkey turns
    /// pass `true` (the default).
    pub play_cue: bool,
    /// If `Some(secs)`, the turn begins with an onset-gated mic open
    /// of `secs` seconds: speech must be detected within that window
    /// or the turn returns `Ok(None)`. When speech *is* detected the
    /// utterance is captured up to `cfg.record.max_seconds`, with the
    /// leading edge preserved.
    ///
    /// `None` (default) preserves the legacy behavior used by `jarvis
    /// listen` and the first wake-triggered turn: open `record_to_wav`
    /// for `cfg.record.max_seconds` and let ffmpeg's trailing-silence
    /// detector decide when to stop.
    pub wait_for_onset_secs: Option<f32>,
}

impl TurnOptions {
    /// Default options for the primary wake/hotkey turn: cue on, no
    /// onset gate, no record override.
    pub fn primary() -> Self {
        Self {
            record_override: None,
            play_cue: true,
            wait_for_onset_secs: None,
        }
    }

    /// Options for a follow-up turn driven by `daemon::run_followup_chain`:
    /// no cue, onset gate of `onset_secs`, no record override. The
    /// daemon uses this directly so the wiring is canonical (one place
    /// to change if follow-up needs another knob).
    pub fn followup(onset_secs: f32) -> Self {
        Self {
            record_override: None,
            play_cue: false,
            wait_for_onset_secs: Some(onset_secs),
        }
    }
}

/// Run a single voice-assistant turn with default options (cue on, normal
/// record config). Convenience wrapper around [`run_turn`] so existing
/// callers (`jarvis listen`, the daemon's first turn) keep their tight
/// signature.
pub fn run_once(cfg: &JarvisConfig) -> Result<Option<String>> {
    run_turn(cfg, TurnOptions::primary())
}

/// Run a single voice-assistant turn. Returns the spoken reply (or `None` if
/// nothing was transcribed).
///
/// `opts` lets callers tweak per-turn behavior — most notably, the daemon's
/// follow-up loop skips the cue and shortens the recording window. See
/// [`TurnOptions`] for the available knobs.
pub fn run_turn(cfg: &JarvisConfig, opts: TurnOptions) -> Result<Option<String>> {
    let stt_engine = stt::build(cfg.stt.clone())?;
    let agent = agents::build(cfg.agent.clone())?;
    let tts_engine = if cfg.speak_responses {
        Some(tts::build(cfg.tts.clone())?)
    } else {
        None
    };

    // Audible cue + buffer settle: gives the user a clear "I'm listening
    // now" signal AND lets PulseAudio's input buffer drain. Without the
    // delay, the new ffmpeg started for `record_to_wav` reads whatever
    // audio was buffered during the wake-detection window — so the tail
    // of the wake utterance ("...por favor") leaks into the command
    // transcript. The cue runs synchronously so the delay is a natural
    // by-product of speaking the prompt rather than a dead pause.
    //
    // Follow-up turns (spec 0007) skip the cue: we are already
    // mid-conversation, the cue would add friction instead of confidence.
    if opts.play_cue {
        play_listening_cue();
        thread::sleep(Duration::from_millis(250));
    }

    let record_cfg = opts.record_override.as_ref().unwrap_or(&cfg.record);

    info!("recording");
    let wav = if let Some(onset_secs) = opts.wait_for_onset_secs {
        // Spec 0007 v1.1: follow-up turns use an onset-gated recorder.
        // It waits up to `onset_secs` for speech to start, then captures
        // the full utterance bounded by the *recorder's* max_seconds —
        // not the follow-up window itself. This fixes the v1 bug where
        // setting `record.max_seconds = followup_window_secs` cut users
        // off mid-sentence.
        match recorder::record_with_onset(record_cfg, onset_secs)? {
            Some(path) => path,
            None => {
                info!("follow-up: no speech within onset window");
                return Ok(None);
            }
        }
    } else {
        recorder::record_to_wav(record_cfg)?
    };

    let result = (|| -> Result<Option<String>> {
        info!("transcribing");
        let prompt = stt_engine.transcribe(&wav)?;
        if prompt.is_empty() {
            warn!("no speech transcribed; aborting turn");
            return Ok(None);
        }
        info!(heard = %prompt, "user said");

        // Load the session up front so the dispatcher's matchers can
        // consult prior `active_workers` state and history when
        // resolving follow-ups (spec D + hija A contract).
        let mut sess = if cfg.session.enabled {
            session::load_or_new(cfg.session.ttl_seconds)?
        } else {
            session::Session::new()
        };
        sess.truncate_to(cfg.session.max_turns);
        let history = sess.turns.clone();
        info!(
            session_id = %sess.id,
            history_turns = history.len(),
            "session loaded"
        );

        // Spec 0010 (orchestrator A): build the dispatcher cascade
        // and let it pick the worker for this turn. Stage 1 is the
        // built-in handlers (time, calc, spec, session-reset, etc.)
        // matching deterministic phrases. Stage 2 is the optional
        // LLM dispatcher (spec 0013 / hija B) — installed only when
        // `[dispatcher.fallback]` is configured; absent that, the
        // cascade keeps the v1 two-stage shape. Stage 3 is the
        // configured default worker — almost always `cfg.agent.name`.
        let mut registry = WorkerRegistry::load_from_dir(
            &cfg_mod::workers_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        let matchers = handlers::register_builtins(&mut registry, cfg);
        let dispatcher = build_cascade(cfg, matchers, llm_stage(cfg));

        let decision = dispatcher
            .dispatch(&prompt, &sess, &registry)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "dispatcher returned None — cascade is mis-configured (no default stage)"
                )
            })?;
        info!(
            worker = %decision.worker_id,
            session_id = ?decision.session_id,
            "dispatched"
        );

        // Resolve and invoke. Built-in handlers and the bundled
        // claude manifest live in the registry; non-claude legacy
        // agents (openai, gemini, warp, shell) don't have manifests
        // yet (deferred from spec C), so we keep their
        // `Agent`-trait path alive as a fallback.
        // Spec 0011 / E1-5: async trigger detection. If the
        // user's utterance contains an "avísame cuando termine"
        // style phrase AND the chosen worker is async-eligible
        // (manifest flag), spawn the worker as a background
        // task instead of waiting synchronously. The user gets
        // an immediate TTS acknowledgement; the supervisor
        // thread fires an OS notification when the worker
        // eventually exits.
        let async_trigger_present = crate::tasks::is_async_trigger(&prompt);
        let worker_handle = registry.get(&decision.worker_id);
        let async_eligible_worker = worker_handle
            .as_ref()
            .map(|w| w.async_eligible())
            .unwrap_or(false);

        if async_trigger_present && async_eligible_worker {
            // Safe to unwrap: the eligibility check above
            // confirmed `worker_handle` is Some.
            let worker = worker_handle.as_ref().unwrap();
            let task_dir = match crate::tasks::TaskRegistry::default_dir() {
                Ok(d) => d,
                Err(e) => return Err(e).context("resolving tasks dir"),
            };
            let (task, _supervisor) = crate::tasks::spawn_async_task(
                worker.as_ref(),
                &WorkerInvocation {
                    prompt: &decision.resolved_prompt,
                    session_id: decision.session_id.as_deref(),
                    cwd: None,
                },
                &task_dir,
                &sess.id,
                &prompt,
            )
            .with_context(|| format!("spawning async task for worker {:?}", decision.worker_id))?;
            info!(task_id = %task.id, "async task spawned for trigger phrase");

            let ack = format!("Listo, te aviso cuando {} termine.", decision.worker_id);

            // Persist a synthetic turn pair: the user's prompt and
            // Jarvis's "te aviso" ack. The actual worker reply
            // goes to the task's stdout.txt and surfaces via OS
            // notification — not into session.json. Voice-driven
            // task queries are spec E2's job.
            if cfg.session.enabled {
                sess.add_turn_for_worker(
                    Role::User,
                    prompt.clone(),
                    decision.worker_id.clone(),
                    decision.session_id.clone(),
                );
                sess.add_turn_for_worker(
                    Role::Assistant,
                    ack.clone(),
                    decision.worker_id.clone(),
                    decision.session_id.clone(),
                );
                sess.set_active_worker_session(
                    decision.worker_id.clone(),
                    decision.session_id.clone(),
                );
                sess.truncate_to(cfg.session.max_turns);
                if let Err(e) = session::save(&sess) {
                    warn!(error = %e, "failed to persist session — continuing");
                }
            }

            if let Some(tts) = &tts_engine
                && !ack.is_empty()
            {
                tts.speak(&ack)?;
            }
            return Ok(Some(ack));
        }

        let response = if let Some(worker) = worker_handle.as_ref() {
            worker
                .invoke(&WorkerInvocation {
                    prompt: &decision.resolved_prompt,
                    session_id: decision.session_id.as_deref(),
                    cwd: None,
                })
                .with_context(|| format!("worker {:?} failed", decision.worker_id))?
        } else {
            // Legacy Agent fallback: still requires history-embedded
            // prompt because non-claude agents are stateless from
            // Jarvis's POV.
            let text = agent
                .respond(&decision.resolved_prompt, &history)
                .with_context(|| format!("legacy agent {} failed", agent.name()))?;
            WorkerResponse {
                text,
                captured_session_id: agent.current_session_id(),
            }
        };
        let reply = response.text.clone();
        info!(reply = %reply, "agent replied");

        // Persist the turn pair with full dispatch metadata.
        if cfg.session.enabled {
            // For stateful workers that captured a new session id
            // mid-invocation (via session_id_capture rules),
            // prefer that; otherwise carry the pre-invocation id
            // through. Either way `worker_session_id` on the
            // recorded turn reflects what was actually used.
            let effective_session_id = response
                .captured_session_id
                .clone()
                .or_else(|| decision.session_id.clone());
            sess.add_turn_for_worker(
                Role::User,
                prompt.clone(),
                decision.worker_id.clone(),
                effective_session_id.clone(),
            );
            sess.add_turn_for_worker(
                Role::Assistant,
                reply.clone(),
                decision.worker_id.clone(),
                effective_session_id.clone(),
            );
            sess.set_active_worker_session(decision.worker_id.clone(), effective_session_id);
            sess.truncate_to(cfg.session.max_turns);
            if let Err(e) = session::save(&sess) {
                warn!(error = %e, "failed to persist session — continuing");
            }
        }

        if let Some(tts) = &tts_engine {
            if !reply.is_empty() {
                tts.speak(&reply)?;
            }
        }
        Ok(Some(reply))
    })();

    // Always clean up the WAV — even on error. We only keep the recording
    // around if the user explicitly asks (future flag).
    let _ = fs::remove_file(&wav);
    result
}

/// Process-wide LLM dispatcher (spec 0013 / hija B). Constructed at
/// most once per daemon process so the 60s classification cache
/// (`LlmDispatcher`'s internal `Mutex<HashMap>`) survives across
/// turns; rebuilding per-turn would drop the cache before it ever
/// got a chance to hit. Initialized on the first `run_turn` that
/// has `[dispatcher.fallback]` configured.
///
/// `None` means "no stage-2 dispatcher" — either because the config
/// section is absent, or because it was malformed at startup and
/// `build_llm_stage` returned `Err`. Both cases produce the same
/// "v1 two-stage cascade" behaviour the spec mandates.
static LLM_STAGE: OnceLock<Option<Arc<LlmDispatcher>>> = OnceLock::new();

/// Build the stage-2 dispatcher from `cfg.dispatcher.fallback` *or*
/// return `None`. Pure function: no global state, no caching. The
/// `OnceLock`-backed [`llm_stage`] wraps this for production use;
/// tests call this directly so each `JarvisConfig` shape gets a
/// fresh evaluation (the OnceLock would otherwise pin the first
/// result for the rest of the process).
///
/// Soft-fails: `Some(invalid_toml)` → log + `None`. The daemon
/// still starts with the v1 cascade shape.
fn try_build_llm_stage(cfg: &JarvisConfig) -> Option<Arc<LlmDispatcher>> {
    let raw = cfg.dispatcher.fallback.as_ref()?;
    match build_llm_stage(raw) {
        Ok(d) => {
            info!(
                backend = d.backend_name(),
                "dispatcher stage 2 (LLM) initialised"
            );
            Some(Arc::new(d))
        }
        Err(e) => {
            warn!(
                error = %e,
                "[dispatcher.fallback] malformed; cascade reverts to v1 two-stage shape"
            );
            None
        }
    }
}

/// Resolve the stage-2 LLM dispatcher for this `cfg`. Initializes
/// the [`LLM_STAGE`] OnceLock on first call; returns the same
/// `Arc` thereafter so the cache persists.
fn llm_stage(cfg: &JarvisConfig) -> Option<Arc<LlmDispatcher>> {
    LLM_STAGE.get_or_init(|| try_build_llm_stage(cfg)).clone()
}

/// Assemble the dispatcher cascade for one turn. Pulled out of
/// `run_turn` so tests can verify the conditional stage-2
/// insertion without spinning up STT / TTS / agents. The cascade
/// always has stages 1 (built-in matchers) and 3 (default
/// worker); stage 2 (LLM) is conditional on `llm_stage` being
/// `Some`.
fn build_cascade(
    cfg: &JarvisConfig,
    matchers: Vec<Arc<dyn IntentMatcher>>,
    llm_stage: Option<Arc<LlmDispatcher>>,
) -> CascadeDispatcher {
    let mut cascade =
        CascadeDispatcher::new().push(Box::new(BuiltinIntentDispatcher::from_matchers(matchers)));
    if let Some(llm) = llm_stage {
        cascade = cascade.push(Box::new(LlmStageHandle(llm)));
    }
    cascade.push(Box::new(DefaultWorkerDispatcher::new(
        cfg.agent.name.clone(),
    )))
}

/// Adapter that lets `Arc<LlmDispatcher>` be pushed onto the
/// `CascadeDispatcher`'s `Vec<Box<dyn Dispatcher>>`. We can't push
/// `Box::new(arc)` directly because `Arc<T>` doesn't auto-deref to
/// `&dyn Dispatcher` in trait-object position; the newtype is the
/// canonical workaround. Cheap — just a `Deref` indirection per
/// dispatch call.
struct LlmStageHandle(Arc<LlmDispatcher>);

impl Dispatcher for LlmStageHandle {
    fn dispatch(
        &self,
        prompt: &str,
        session: &crate::session::Session,
        registry: &WorkerRegistry,
    ) -> Result<Option<crate::dispatcher::DispatchDecision>> {
        self.0.dispatch(prompt, session, registry)
    }
}

/// Best-effort audible "I'm listening" cue. Tries espeak-ng (universally
/// available on Linux, instant, no model dependency), then falls back to
/// the terminal bell if it's missing. Errors are silently swallowed —
/// missing audio cue is a UX nicety, not a turn-blocker.
fn play_listening_cue() {
    if which::which("espeak-ng").is_ok() {
        let _ = Command::new("espeak-ng")
            .args(["-s", "300", "-a", "120", "si"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return;
    }
    if which::which("espeak").is_ok() {
        let _ = Command::new("espeak")
            .args(["-s", "300", "-a", "120", "si"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return;
    }
    // Terminal bell fallback. Quietly does nothing if the terminal
    // suppresses it (and in daemon mode there might not even be a TTY).
    use std::io::Write;
    let _ = std::io::stderr().write_all(b"\x07");
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 0007: the primary-turn options must play the cue and not
    /// override the recorder. This is the contract `jarvis listen` and
    /// the daemon's wake-triggered first turn rely on; the follow-up
    /// chain explicitly diverges and is verified by reading the daemon
    /// code (it constructs a `TurnOptions` with `play_cue: false` and a
    /// shortened record override).
    #[test]
    fn primary_turn_options_play_cue_and_use_default_recorder() {
        let opts = TurnOptions::primary();
        assert!(opts.play_cue);
        assert!(opts.record_override.is_none());
    }

    /// Spec 0007: a zero-value `Default` is intentionally muted — it
    /// represents "no cue, no override" which is what a follow-up turn
    /// wants. Catches accidental `play_cue: true` slipping back into
    /// the `Default` impl during refactors.
    #[test]
    fn default_turn_options_match_followup_shape() {
        let opts = TurnOptions::default();
        assert!(!opts.play_cue);
        assert!(opts.record_override.is_none());
    }

    /// Spec 0013 / B-4: `[dispatcher.fallback]` absent → no stage 2.
    /// The pipeline still runs (just with the v1 two-stage cascade).
    /// This is the zero-config default every existing install gets.
    #[test]
    fn try_build_llm_stage_returns_none_when_fallback_absent() {
        let cfg = JarvisConfig::default();
        assert!(cfg.dispatcher.fallback.is_none());
        assert!(try_build_llm_stage(&cfg).is_none());
    }

    /// Spec 0013 / B-4: a well-formed `[dispatcher.fallback]`
    /// produces a stage-2 dispatcher. The dispatcher's backend name
    /// confirms which backend got picked, which is also the
    /// surface `jarvis dispatcher status` (future) will report.
    #[test]
    fn try_build_llm_stage_returns_some_for_valid_oz_config() {
        let mut cfg = JarvisConfig::default();
        cfg.dispatcher.fallback = Some(toml::Value::Table(toml::toml! {
            backend = "oz"
            model = "test-model"
        }));
        let stage = try_build_llm_stage(&cfg).expect("should build");
        assert_eq!(stage.backend_name(), "oz");
    }

    /// Same for the HTTP backend — verifies both branches of the
    /// `match backend` in `build_llm_stage` go through the
    /// pipeline helper cleanly.
    #[test]
    fn try_build_llm_stage_returns_some_for_valid_openai_compat_config() {
        let mut cfg = JarvisConfig::default();
        cfg.dispatcher.fallback = Some(toml::Value::Table(toml::toml! {
            backend = "openai_compat"
            endpoint = "http://localhost:11434/v1/chat/completions"
            model = "llama-3.1-8b"
        }));
        let stage = try_build_llm_stage(&cfg).expect("should build");
        assert_eq!(stage.backend_name(), "openai_compat");
    }

    /// Spec 0013 / B-4: malformed `[dispatcher.fallback]` returns
    /// `None` rather than propagating the error. The daemon
    /// boots, the cascade reverts to v1 two-stage shape, and the
    /// error surfaces only as a tracing WARN. Critical invariant:
    /// a typo in the dispatcher fallback config must never brick
    /// the daemon.
    #[test]
    fn try_build_llm_stage_soft_fails_on_malformed_fallback() {
        let mut cfg = JarvisConfig::default();
        cfg.dispatcher.fallback = Some(toml::Value::Table(toml::toml! {
            backend = "magic-router"
            model = "x"
        }));
        assert!(
            try_build_llm_stage(&cfg).is_none(),
            "unknown backend should soft-fail to None"
        );
    }

    /// Missing required fields (here: oz without `model`) also
    /// soft-fails to `None`. Same daemon-keeps-booting contract.
    #[test]
    fn try_build_llm_stage_soft_fails_on_missing_required_field() {
        let mut cfg = JarvisConfig::default();
        cfg.dispatcher.fallback = Some(toml::Value::Table(toml::toml! {
            backend = "oz"
        }));
        assert!(try_build_llm_stage(&cfg).is_none());
    }

    /// Spec 0013 / B-4: cascade omits stage 2 when no LLM
    /// dispatcher is supplied. The v1 cascade shape — built-in
    /// matchers + default worker — is what every existing install
    /// currently runs, and it must stay reachable.
    #[test]
    fn cascade_has_two_stages_without_llm() {
        let cfg = JarvisConfig::default();
        let cascade = build_cascade(&cfg, Vec::new(), None);
        assert_eq!(cascade.stage_count(), 2, "matcher + default; no LLM stage");
    }

    /// Spec 0013 / B-4: cascade adds stage 2 when an LLM
    /// dispatcher is supplied. End-to-end shape verification —
    /// the cascade tests in `dispatcher::cascade` cover ordering;
    /// here we verify the pipeline's *insertion* of the stage.
    #[test]
    fn cascade_has_three_stages_with_llm() {
        use crate::dispatcher::llm::testing::MockLlmBackend;
        let cfg = JarvisConfig::default();
        let llm = Arc::new(LlmDispatcher::new(Box::new(MockLlmBackend::declining())));
        let cascade = build_cascade(&cfg, Vec::new(), Some(llm));
        assert_eq!(
            cascade.stage_count(),
            3,
            "matcher + LLM + default; stage 2 inserted"
        );
    }

    /// End-to-end dispatch through the assembled cascade: stage 1
    /// declines, stage 2's mock backend picks `claude`, the cascade
    /// returns that decision rather than falling through to the
    /// default. Exercises the wire-through that B-4 establishes;
    /// without the LlmStageHandle adapter, the cascade couldn't
    /// even hold the LlmDispatcher.
    #[test]
    fn cascade_routes_through_llm_stage_to_picked_worker() {
        use crate::dispatcher::llm::testing::MockLlmBackend;
        use crate::session::Session;
        use crate::workers::{WorkerHandle, WorkerInvocation, WorkerRegistry, WorkerResponse};

        struct FakeClaude;
        impl WorkerHandle for FakeClaude {
            fn id(&self) -> &str {
                "claude"
            }
            fn invoke(&self, _: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
                unreachable!("test should not invoke")
            }
        }

        let cfg = JarvisConfig::default();
        let llm = Arc::new(LlmDispatcher::new(Box::new(MockLlmBackend::picking(
            "claude",
        ))));
        let cascade = build_cascade(&cfg, Vec::new(), Some(llm));

        let mut registry = WorkerRegistry::default();
        registry.register_builtin(Arc::new(FakeClaude));
        let session = Session::new();

        let decision = cascade
            .dispatch("explícame los protocolos de gossip", &session, &registry)
            .unwrap()
            .expect("stage 2 picks claude");
        assert_eq!(decision.worker_id, "claude");
    }

    /// When stage 2's backend declines, the cascade falls through
    /// to the default worker (stage 3). Confirms the cascade
    /// shape doesn't swallow the prompt: a declining LLM
    /// dispatcher must produce the same outcome as no LLM
    /// dispatcher at all.
    #[test]
    fn cascade_falls_through_to_default_when_llm_declines() {
        use crate::dispatcher::llm::testing::MockLlmBackend;
        use crate::session::Session;
        use crate::workers::WorkerRegistry;

        let cfg = JarvisConfig::default();
        let llm = Arc::new(LlmDispatcher::new(Box::new(MockLlmBackend::declining())));
        let cascade = build_cascade(&cfg, Vec::new(), Some(llm));

        let registry = WorkerRegistry::default();
        let session = Session::new();
        let decision = cascade
            .dispatch("anything", &session, &registry)
            .unwrap()
            .expect("should fall through to default");
        assert_eq!(
            decision.worker_id, cfg.agent.name,
            "stage 3 default worker claims the turn"
        );
    }

    /// Same shape when stage 2's backend errors: cascade falls
    /// through, default claims. Critical contract — a transient
    /// classifier failure must not kill the turn.
    #[test]
    fn cascade_falls_through_to_default_when_llm_errors() {
        use crate::dispatcher::llm::testing::MockLlmBackend;
        use crate::session::Session;
        use crate::workers::WorkerRegistry;

        let cfg = JarvisConfig::default();
        let llm = Arc::new(LlmDispatcher::new(Box::new(MockLlmBackend::failing(
            "classifier down",
        ))));
        let cascade = build_cascade(&cfg, Vec::new(), Some(llm));

        let registry = WorkerRegistry::default();
        let session = Session::new();
        let decision = cascade
            .dispatch("anything", &session, &registry)
            .unwrap()
            .expect("must not propagate the backend error");
        assert_eq!(decision.worker_id, cfg.agent.name);
    }
}
