//! One-shot record → STT → agent → TTS turn.
//!
//! Both `jarvis listen` (hotkey-triggered) and the wake-word daemon call
//! [`run_once`]. Keeping the orchestration in one place ensures both entry
//! points go through the same code path; no chance of one drifting from the
//! other as the project grows.

use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

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

    // Audible cue + buffer settle: gives the user a clear "I'm listening
    // now" signal AND lets PulseAudio's input buffer drain. Without the
    // delay, the new ffmpeg started for `record_to_wav` reads whatever
    // audio was buffered during the wake-detection window — so the tail
    // of the wake utterance ("...por favor") leaks into the command
    // transcript. The cue runs synchronously so the delay is a natural
    // by-product of speaking the prompt rather than a dead pause.
    play_listening_cue();
    thread::sleep(Duration::from_millis(250));

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
