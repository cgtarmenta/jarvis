//! Voice triggers that ask Jarvis to run a task in the background.
//!
//! Spec 0011 / E1-5: the pipeline scans every prompt for a
//! short list of "...and let me know when it's done" phrases
//! before deciding whether to invoke the chosen worker
//! synchronously or via [`crate::tasks::spawn_async_task`]. The
//! trigger is a *modifier*, not an intent — it doesn't change
//! which worker handles the request, only whether the user is
//! prepared to wait for the reply or wants the daemon to fire
//! an OS notification later.

/// Substring triggers. Matched against the normalised prompt
/// (lowercase, accent-stripped, punctuation stripped, whitespace
/// collapsed). Substring rather than exact equality because the
/// trigger lives inside a longer instruction: "analiza el log y
/// avísame cuando termines".
///
/// Substring matching means a few question-shaped Spanish phrases
/// like "¿cuándo termines el sprint?" will technically claim the
/// trigger and spawn an async task. We accept this trade-off
/// because (a) such phrases are rare in actual voice usage —
/// users asking a question phrase it as "¿cuándo terminas?", not
/// "termines"; (b) the worst case is a real answer that arrives
/// via OS notification instead of TTS, which is still a useful
/// reply; and (c) a future intent-aware classifier (hija B's LLM
/// dispatcher) can disambiguate when needed.
const ASYNC_TRIGGERS: &[&str] = &[
    // Spanish
    "avisame cuando",
    "avisame al",
    "cuando termines",
    "cuando acabes",
    "cuando hayas terminado",
    "cuando hayas acabado",
    "dejalo en background",
    "dejalo corriendo",
    "asincronicamente",
    "asincrono",
    "en segundo plano",
    "en background",
    // English
    "let me know when",
    "notify me when",
    "tell me when",
    "in the background",
    "when you finish",
    "when you're done",
    "when youre done",
    "when done",
    "when complete",
    "when completed",
    "run it in the background",
];

/// Returns `true` if any documented trigger appears in the
/// normalised form of `prompt`.
pub fn is_async_trigger(prompt: &str) -> bool {
    let n = normalise(prompt);
    if n.is_empty() {
        return false;
    }
    ASYNC_TRIGGERS.iter().any(|t| n.contains(t))
}

/// Same normalisation the other built-in handlers use:
/// accent-stripped, lowercased, punctuation removed, whitespace
/// collapsed. Inline copy because crate-level helpers would be
/// premature for one consumer here and one in `handlers/`.
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

    /// Each documented trigger phrase, embedded in a realistic
    /// surrounding sentence, fires the detector.
    #[test]
    fn realistic_phrases_trigger() {
        for phrase in [
            "analiza el log y avísame cuando termines",
            "ejecuta esto y avísame cuando acabes",
            "corre la prueba y déjalo en background",
            "déjalo corriendo y sigue otra cosa",
            "haz esto asincronicamente por favor",
            "lánzalo en segundo plano",
            "make a build and let me know when it's done",
            "run the analysis and notify me when complete",
            "tell me when you're done",
            "put this in the background",
        ] {
            assert!(is_async_trigger(phrase), "should trigger: {phrase:?}");
        }
    }

    /// Prompts that don't carry the trigger leave the pipeline
    /// on its synchronous path. This is the safety check: we'd
    /// rather miss an async-intent than mis-spawn a turn.
    #[test]
    fn non_trigger_prompts_decline() {
        for phrase in [
            "qué hora es",
            "abre un spec para X",
            "explícame qué es un Triton server",
            "hola",
            "olvida todo",
            "cuánto es 5 por 3",
            // Has "cuando" but not part of any trigger phrase.
            "qué hago cuando llueve",
        ] {
            assert!(!is_async_trigger(phrase), "should NOT trigger: {phrase:?}");
        }
    }

    /// Empty / whitespace prompts decline cleanly.
    #[test]
    fn empty_input_declines() {
        assert!(!is_async_trigger(""));
        assert!(!is_async_trigger("   "));
        assert!(!is_async_trigger("\n\t"));
    }

    /// Case + accent + punctuation insensitivity. The
    /// normaliser handles all three so triggers fire against
    /// whatever shape Whisper happens to emit.
    #[test]
    fn case_accent_and_punctuation_insensitive() {
        assert!(is_async_trigger("Avísame cuando termines."));
        assert!(is_async_trigger("AVISAME CUANDO TERMINES"));
        assert!(is_async_trigger("avisame, cuando termines!"));
        assert!(is_async_trigger("Notify Me When done"));
    }
}
