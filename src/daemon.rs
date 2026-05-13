//! Daemon mode — wake-word loop and signal handling.
//!
//! Most users should bind `jarvis listen` to a hotkey and skip the daemon
//! entirely. The daemon exists for hands-free setups: when
//! `[wake] enabled = true` it loads the configured wake backend and runs an
//! always-on listener that invokes `pipeline::run_once` on each wake event.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use signal_hook::consts::{SIGINT, SIGTERM};
use tracing::info;

use crate::config::JarvisConfig;
use crate::pipeline::{TurnOptions, run_once, run_turn};
use crate::wake;

pub fn run(cfg: JarvisConfig) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, stop.clone())?;
    signal_hook::flag::register(SIGTERM, stop.clone())?;

    if !cfg.wake.enabled {
        info!(
            "wake-word mode is disabled in config. The daemon has nothing to \
             do — bind `jarvis listen` to a hotkey, or set [wake] enabled = \
             true and pick a backend."
        );
        return Ok(());
    }

    let backend = wake::build(cfg.wake.clone(), cfg.stt.clone())?;
    info!(
        backend = backend.name(),
        phrases = ?cfg.wake.phrases,
        "Jarvis daemon ready"
    );

    let cfg_for_callback = cfg.clone();
    let stop_for_cb = Arc::clone(&stop);
    let stop_for_check = Arc::clone(&stop);
    let mut wake_cb = move || {
        if stop_for_cb.load(Ordering::Relaxed) {
            return;
        }
        if let Err(err) = run_once(&cfg_for_callback) {
            // One bad turn shouldn't kill the daemon — log and keep going.
            tracing::error!("turn failed: {err:#}");
            return;
        }
        // Spec 0007: follow-up listening. After a wake-triggered turn,
        // keep the mic open for a short window so short clarifications
        // ("¿y en Tokio?") don't require re-saying the wake word.
        // Empty follow-up captures or any error terminate the chain
        // and return us to wake-word gating.
        run_followup_chain(&cfg_for_callback, &stop_for_cb);
    };

    backend.run(&mut wake_cb, &|| stop_for_check.load(Ordering::Relaxed))
}

/// Loop on follow-up turns until the user goes silent (`run_turn` returns
/// `Ok(None)`), an error occurs, or shutdown is requested.
///
/// Each follow-up turn uses an onset-gated recorder: we wait up to
/// `session.followup_window_secs` for the user to start speaking, and if
/// they do, we capture the *entire* utterance bounded by the normal
/// `record.max_seconds` — not by the follow-up window. The v1 of this
/// loop conflated the two timeouts and cut users off mid-sentence; see
/// the spec journal for the post-mortem.
fn run_followup_chain(cfg: &JarvisConfig, stop: &Arc<AtomicBool>) {
    let window = cfg.session.followup_window_secs;
    if window <= 0.0 {
        return;
    }
    let opts = TurnOptions::followup(window);
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match run_turn(cfg, opts.clone()) {
            Ok(Some(_)) => continue,
            Ok(None) => return,
            Err(err) => {
                tracing::error!("follow-up turn failed: {err:#}");
                return;
            }
        }
    }
}
