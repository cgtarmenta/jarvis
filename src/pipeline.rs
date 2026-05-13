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
use crate::session::{self, Role};
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

        // Reset-phrase short-circuit: if the entire user utterance matches
        // one of the configured phrases (case-insensitive, accent-stripped),
        // wipe the session and confirm. We never forward reset commands to
        // the agent — the user said "forget" to *us*, not to Claude.
        if cfg.session.enabled && is_reset_phrase(&prompt, &cfg.session.reset_phrases) {
            session::reset()?;
            info!("session reset by voice command");
            if let Some(tts) = &tts_engine {
                let confirmation = "Listo, empezamos de nuevo.";
                tts.speak(confirmation)?;
                return Ok(Some(confirmation.to_string()));
            }
            return Ok(Some(String::from("Session reset.")));
        }

        // Spec-management intent: "abre un spec para X", "promote 14",
        // etc. These are handled deterministically — never forwarded to
        // the agent because filesystem mutations should not be the LLM's
        // job. The handler returns a short TTS-friendly summary.
        if let Some(intent) = crate::specs::recognize(&prompt) {
            info!(?intent, "spec intent recognised");
            let reply = crate::specs::execute(intent);
            if let Some(tts) = &tts_engine
                && !reply.is_empty()
            {
                tts.speak(&reply)?;
            }
            return Ok(Some(reply));
        }

        // Load (or implicitly create) the active session. Truncate before
        // sending so we don't blow the model's context window — keeping
        // only the most recent `max_turns` turns.
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

        let reply = agent
            .respond(&prompt, &history)
            .with_context(|| format!("agent {} failed", agent.name()))?;
        info!(reply = %reply, "agent replied");

        // Persist the turn pair. We save *after* the agent call so a
        // failure in the agent path doesn't pollute the session with an
        // unanswered user turn.
        if cfg.session.enabled {
            sess.add_turn(Role::User, prompt.clone());
            sess.add_turn(Role::Assistant, reply.clone());
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

/// Match `prompt` against any of the configured reset phrases after
/// normalising both sides (lowercase, accent-stripped, trimmed). We
/// require an **exact** match on the whole utterance — otherwise a
/// question like "puedes olvidar la última cosa?" would nuke the session.
fn is_reset_phrase(prompt: &str, phrases: &[String]) -> bool {
    let normalised = normalise(prompt);
    if normalised.is_empty() {
        return false;
    }
    phrases.iter().any(|p| normalise(p) == normalised)
}

fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
