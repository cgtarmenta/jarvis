//! Built-in handler for "reset the session" voice phrases.
//!
//! Carries the same matching logic that lived in
//! `pipeline::run_turn`'s `is_reset_phrase` + `normalise` helpers,
//! relocated here so the dispatcher cascade can route to it
//! uniformly. The inline pipeline check stays in place until A-4
//! wires the dispatcher; this handler is registered but unused
//! until then.

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

/// Phrases like "olvida todo", "nueva conversación", "reset". The
/// list is sourced from `[session].reset_phrases` in the user's
/// config so it can be localised without code changes. Matching is
/// exact-equality on the normalised form (lowercase, accent-stripped,
/// punctuation removed, whitespace collapsed) — substring matching
/// was rejected up-front because "¿puedes olvidar la última cosa?"
/// would otherwise wipe the session unintentionally.
pub struct SessionResetHandler {
    phrases: Vec<String>,
}

impl SessionResetHandler {
    pub fn new(phrases: Vec<String>) -> Self {
        Self { phrases }
    }
}

impl IntentMatcher for SessionResetHandler {
    fn worker_id(&self) -> &str {
        "session-reset"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let normalised = normalise(prompt);
        if normalised.is_empty() {
            return None;
        }
        if self.phrases.iter().any(|p| normalise(p) == normalised) {
            // Resolved prompt is the original — invoke ignores it
            // because the reset is unconditional once matched.
            Some(prompt.to_string())
        } else {
            None
        }
    }
}

impl WorkerHandle for SessionResetHandler {
    fn id(&self) -> &str {
        "session-reset"
    }

    fn description(&self) -> Option<&str> {
        Some("Resets the active conversation session.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user explicitly asks to forget the conversation, \
             start over, or reset memory.",
        )
    }

    fn invoke(&self, _ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        crate::session::reset()?;
        Ok(WorkerResponse {
            // Matches the legacy ClaudeAgent's reset confirmation —
            // keeping the user-visible string identical avoids a
            // surprise behaviour change when A-4 swaps the inline
            // pipeline check for the dispatcher.
            text: "Listo, empezamos de nuevo.".to_string(),
            captured_session_id: None,
        })
    }
}

/// Normalise a phrase for case-insensitive, accent-stripped,
/// whitespace-collapsed comparison. Mirrors the helper that used to
/// live in `pipeline.rs` so the matching behaviour is bit-for-bit
/// identical with what pre-A-2 users have today.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> SessionResetHandler {
        // Mirrors the SessionConfig::default() phrase list.
        SessionResetHandler::new(vec![
            "olvida".to_string(),
            "olvidalo".to_string(),
            "olvida todo".to_string(),
            "nueva conversacion".to_string(),
            "nueva conversación".to_string(),
            "new conversation".to_string(),
            "forget".to_string(),
            "forget everything".to_string(),
            "reset".to_string(),
        ])
    }

    /// Each configured phrase, in its plain form, matches.
    #[test]
    fn exact_phrases_match() {
        let h = handler();
        let session = Session::new();
        for phrase in [
            "olvida",
            "olvida todo",
            "nueva conversación",
            "reset",
            "forget everything",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise {phrase:?}"
            );
        }
    }

    /// Substring matches do NOT trigger the reset — "¿puedes olvidar
    /// la última cosa?" is a real question, not a reset command.
    /// This is the bit-for-bit safety guarantee we inherited from
    /// pipeline.rs and want to preserve.
    #[test]
    fn substrings_do_not_match() {
        let h = handler();
        let session = Session::new();
        assert!(
            h.recognize("puedes olvidar la última cosa", &session)
                .is_none()
        );
        assert!(h.recognize("voy a hacer reset luego", &session).is_none());
        assert!(h.recognize("no olvides comprar pan", &session).is_none());
    }

    /// Accent / capitalisation insensitivity — the normaliser
    /// strips both before comparing.
    #[test]
    fn case_and_accent_insensitive() {
        let h = handler();
        let session = Session::new();
        assert!(h.recognize("OLVIDA", &session).is_some());
        assert!(h.recognize("nueva conversación", &session).is_some());
        assert!(h.recognize("Nueva Conversación", &session).is_some());
        assert!(h.recognize("nueva conversacion", &session).is_some());
    }

    /// Empty prompt → no match. Defends against the boundary case
    /// where a misfired STT produces just whitespace.
    #[test]
    fn empty_prompt_declines() {
        let h = handler();
        let session = Session::new();
        assert!(h.recognize("", &session).is_none());
        assert!(h.recognize("   ", &session).is_none());
    }

    /// IDs match across traits — same invariant the spec handler
    /// holds. The dispatcher relies on this for routing.
    #[test]
    fn ids_match_across_traits() {
        let h = handler();
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}
