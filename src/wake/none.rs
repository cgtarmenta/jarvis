//! "Don't actually listen" backend — preserves the hotkey-only flow.
//!
//! The daemon may still be running so the user can run `jarvis listen`
//! elsewhere; this backend just exits its `run` loop immediately with a
//! friendly message instead of consuming any audio.

use anyhow::Result;
use tracing::info;

use super::WakeBackend;

pub struct NoopWake;

impl WakeBackend for NoopWake {
    fn name(&self) -> &'static str {
        "none"
    }

    fn run(&self, _on_wake: &mut dyn FnMut(), _should_stop: &dyn Fn() -> bool) -> Result<()> {
        info!(
            "wake backend = \"none\" — Jarvis is hotkey-only. \
             Bind `jarvis listen` in your WM and you're set."
        );
        Ok(())
    }
}
