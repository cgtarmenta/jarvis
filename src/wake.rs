//! Wake-word triggering — feature-gated stub.
//!
//! For v1 we ship hotkey-driven `jarvis listen` only. Wake-word is wired as a
//! `wakeword` Cargo feature so the always-listening path can later plug in
//! `rustpotter`, `sherpa-onnx`, or a subprocess wake-word service without
//! changing the rest of the codebase.

use anyhow::{Result, anyhow};

use crate::config::WakeConfig;

/// Block in `run` until the wake word fires, then invoke `on_wake`.
///
/// Today this is a placeholder; calling it with `wake.enabled = true` returns
/// an error so the user gets a clear message rather than silent inaction.
/// The `cfg` field will be used once a real backend is wired in — keeping it
/// here means callers don't have to change their construction code later.
pub struct WakeListener {
    #[allow(dead_code)] // wired in when a wake-word backend is implemented
    cfg: WakeConfig,
}

impl WakeListener {
    pub fn new(cfg: WakeConfig) -> Self {
        Self { cfg }
    }

    #[cfg(feature = "wakeword")]
    pub fn run<F: FnMut()>(&self, _on_wake: F) -> Result<()> {
        // TODO: wire rustpotter or sherpa-onnx here.
        let _ = &self.cfg;
        Err(anyhow!(
            "wake-word backend is not yet implemented — track issue #1"
        ))
    }

    #[cfg(not(feature = "wakeword"))]
    pub fn run<F: FnMut()>(&self, _on_wake: F) -> Result<()> {
        Err(anyhow!(
            "wake-word feature not compiled in. Rebuild with: cargo build --features wakeword"
        ))
    }
}
