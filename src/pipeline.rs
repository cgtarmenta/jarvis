//! One-shot record → STT → agent → TTS turn.
//!
//! Both `jarvis listen` (hotkey-triggered) and the wake-word daemon call
//! [`run_once`]. Keeping the orchestration in one place ensures both entry
//! points go through the same code path; no chance of one drifting from the
//! other as the project grows.

use std::fs;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::agents;
use crate::config::JarvisConfig;
use crate::recorder;
use crate::stt;
use crate::tts;

/// Run a single voice-assistant turn. Returns the spoken reply (or `None` if
/// nothing was transcribed).
pub fn run_once(cfg: &JarvisConfig) -> Result<Option<String>> {
    let stt_engine = stt::build(cfg.stt.clone())?;
    let agent = agents::build(cfg.agent.clone())?;
    let tts_engine = if cfg.speak_responses {
        Some(tts::build(cfg.tts.clone())?)
    } else {
        None
    };

    info!("recording");
    let wav = recorder::record_to_wav(&cfg.record)?;

    let result = (|| -> Result<Option<String>> {
        info!("transcribing");
        let prompt = stt_engine.transcribe(&wav)?;
        if prompt.is_empty() {
            warn!("no speech transcribed; aborting turn");
            return Ok(None);
        }
        info!(heard = %prompt, "user said");

        let reply = agent
            .respond(&prompt)
            .with_context(|| format!("agent {} failed", agent.name()))?;
        info!(reply = %reply, "agent replied");

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
