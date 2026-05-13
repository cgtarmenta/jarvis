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
use crate::pipeline::run_once;
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

    let backend = wake::build(cfg.wake.clone())?;
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
        }
    };

    backend.run(&mut wake_cb, &|| stop_for_check.load(Ordering::Relaxed))
}
