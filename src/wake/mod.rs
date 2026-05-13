//! Wake-word listening — pluggable backends.
//!
//! Mirrors the architecture of `stt` / `tts` / `agents`: a small trait, a
//! factory keyed off `[wake].backend` in `config.toml`, and per-backend files
//! that can be feature-gated. Right now we ship two real backends — `none`
//! (hotkey-only, default) and `whisper` (reuses whisper.cpp for phrase
//! matching). `sherpa`, `openwakeword`, and `rustpotter` are placeholders
//! that surface a clear error until someone wires them up.

use anyhow::{Result, anyhow};

use crate::config::WakeConfig;

mod none;
mod whisper;

pub use none::NoopWake;
pub use whisper::WhisperWake;

/// Implemented by every wake-word backend.
///
/// `run` blocks until either an unrecoverable error occurs or `should_stop`
/// returns `true` between detections. When a wake phrase is recognised it
/// calls `on_wake` synchronously; the backend pauses listening while the
/// callback runs so audio doesn't get double-consumed by the pipeline.
pub trait WakeBackend {
    fn name(&self) -> &'static str;
    fn run(&self, on_wake: &mut dyn FnMut(), should_stop: &dyn Fn() -> bool) -> Result<()>;
}

/// Build the configured backend.
pub fn build(cfg: WakeConfig) -> Result<Box<dyn WakeBackend + Send + Sync>> {
    let backend = cfg.backend.to_lowercase();
    match backend.as_str() {
        "none" | "off" | "disabled" | "" => Ok(Box::new(NoopWake)),
        "whisper" | "whisper-cli" => Ok(Box::new(WhisperWake::new(cfg)?)),

        // Roadmap backends — wired into the factory so the wizard can show
        // them and the user gets a clear error instead of silent failure
        // when picking one. Each gets its own Cargo feature so a future PR
        // can land it without touching the rest of the codebase.
        "sherpa" | "sherpa-onnx" => Err(anyhow!(
            "sherpa wake backend is not yet implemented \
             (see issue tracker; rebuild with --features wake-sherpa when available)"
        )),
        "openwakeword" | "oww" => Err(anyhow!(
            "openwakeword backend is not yet implemented \
             (rebuild with --features wake-openwakeword when available)"
        )),
        "rustpotter" => Err(anyhow!(
            "rustpotter backend is not yet implemented \
             (rebuild with --features wake-rustpotter when available)"
        )),

        other => Err(anyhow!(
            "unknown wake backend: {other:?}. Known: none, whisper, \
             sherpa (roadmap), openwakeword (roadmap), rustpotter (roadmap)"
        )),
    }
}

/// Whether a given backend name is fully implemented today. The wizard uses
/// this to label entries as `(roadmap)` instead of letting users pick
/// something that will fail at runtime.
pub fn is_implemented(backend: &str) -> bool {
    matches!(
        backend.to_lowercase().as_str(),
        "none" | "off" | "disabled" | "" | "whisper" | "whisper-cli"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_backend_is_rejected() {
        let cfg = WakeConfig {
            backend: "fnord".into(),
            ..WakeConfig::default()
        };
        assert!(build(cfg).is_err());
    }

    #[test]
    fn known_backends() {
        assert!(is_implemented("none"));
        assert!(is_implemented("whisper"));
        assert!(!is_implemented("sherpa"));
        assert!(!is_implemented("openwakeword"));
        assert!(!is_implemented("rustpotter"));
    }
}
