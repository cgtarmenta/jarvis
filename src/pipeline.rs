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
use crate::config::{JarvisConfig, RecordConfig};
use crate::recorder;
use crate::session::{self, Role};
use crate::stt;
use crate::tts;

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

        // Spec 0009 (orchestrator D): capture the worker's current
        // session id *before* the call so the turn record reflects
        // what was actually resumed. Stateful agents (Claude with
        // its attach UUID) return Some; stateless agents return None.
        let worker_session_id = agent.current_session_id();
        let worker_id = agent.name().to_string();

        let reply = agent
            .respond(&prompt, &history)
            .with_context(|| format!("agent {} failed", agent.name()))?;
        info!(reply = %reply, "agent replied");

        // Persist the turn pair with full dispatch metadata. We save
        // *after* the agent call so a failure in the agent path
        // doesn't pollute the session with an unanswered user turn.
        if cfg.session.enabled {
            sess.add_turn_for_worker(
                Role::User,
                prompt.clone(),
                worker_id.clone(),
                worker_session_id.clone(),
            );
            sess.add_turn_for_worker(
                Role::Assistant,
                reply.clone(),
                worker_id.clone(),
                worker_session_id.clone(),
            );
            // Record this worker's most recently-known session id in
            // the active_workers map. For Claude this is the attach
            // UUID; for stateless agents it's `None`. Spec D's
            // contract: the map is the dispatcher's per-thread
            // "who's holding which session" registry. Hija A will
            // extend this with session-id-capture for newly-spawned
            // workers; today it's just whatever the agent surfaced
            // pre-invocation, which captures the resume case.
            sess.set_active_worker_session(worker_id, worker_session_id);
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
}
