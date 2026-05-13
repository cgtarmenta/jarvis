//! Daemon mode — wake-word loop and signal handling.
//!
//! Most users should bind `jarvis listen` to a hotkey and skip the daemon
//! entirely. The daemon is for hands-free setups where `[wake] enabled = true`
//! and you want a long-lived process under systemd / launchd that triggers
//! one pipeline turn per detected wake word.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use signal_hook::consts::{SIGINT, SIGTERM};
use tracing::info;

use crate::config::JarvisConfig;
use crate::pipeline::run_once;
use crate::wake::WakeListener;

pub fn run(cfg: JarvisConfig) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, stop.clone())?;
    signal_hook::flag::register(SIGTERM, stop.clone())?;

    if !cfg.wake.enabled {
        info!(
            "wake-word mode is disabled in config. The daemon has nothing to \
             do — bind `jarvis listen` to a hotkey, or set [wake] enabled = \
             true and rebuild with --features wakeword."
        );
        return Ok(());
    }

    let listener = WakeListener::new(cfg.wake.clone());
    let cfg_for_callback = cfg.clone();
    listener.run(move || {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Err(err) = run_once(&cfg_for_callback) {
            // One bad turn shouldn't kill the daemon — log and keep listening.
            tracing::error!("turn failed: {err:#}");
        }
    })
}
