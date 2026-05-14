//! Built-in handler for simple arithmetic via voice.
//!
//! Backed by `evalexpr`, which parses standard infix expressions
//! and accepts the spelled-out operators that voice transcripts
//! commonly produce ("dos más dos", "five times three"). We
//! pre-normalise Spanish/English number words and operator names
//! into their digit/symbol forms before handing the string to
//! `evalexpr`.

use anyhow::Result;
use evalexpr::{Value, eval};

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

const CALC_TRIGGERS: &[&str] = &[
    "cuanto es ",
    "calcula ",
    "calcular ",
    "what is ",
    "whats ",
    "calculate ",
    "compute ",
];

pub struct CalcHandler;

impl IntentMatcher for CalcHandler {
    fn worker_id(&self) -> &str {
        "calc"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        let stripped = CALC_TRIGGERS
            .iter()
            .find_map(|t| n.strip_prefix(t).map(|s| s.trim().to_string()))?;
        if stripped.is_empty() {
            return None;
        }
        // Only claim the turn if the stripped tail looks like
        // arithmetic — i.e. eval-translates to something with at
        // least one digit and one operator. This keeps the
        // dispatcher from grabbing things like "calculate the
        // distance to the moon" that have a trigger word but no
        // numeric content.
        let expr = translate_to_expression(&stripped);
        if !looks_like_arithmetic(&expr) {
            return None;
        }
        Some(stripped)
    }
}

impl WorkerHandle for CalcHandler {
    fn id(&self) -> &str {
        "calc"
    }

    fn description(&self) -> Option<&str> {
        Some("Evaluates simple arithmetic expressions spoken by the user.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use for spoken arithmetic queries like \"cuánto es 5 más 3\" \
             or \"what is 12 times 7\". Refuses non-arithmetic prompts.",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let normalised = normalise(ctx.prompt);
        // Strip the trigger again (cheap) so this method works
        // when called with the resolved prompt (which is already
        // the tail) OR with the original prompt.
        let payload = CALC_TRIGGERS
            .iter()
            .find_map(|t| normalised.strip_prefix(t).map(|s| s.to_string()))
            .unwrap_or(normalised);
        let expr = translate_to_expression(&payload);
        let text = match eval(&expr) {
            Ok(v) => format!("Eso da {}.", render_value(&v)),
            Err(e) => format!("No pude calcular eso: {e}"),
        };
        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

/// Translate spelled-out operators and a few common digit-words
/// into `evalexpr`-friendly symbols. Conservative on purpose: we
/// don't try to fully understand prose; we only swap the
/// vocabulary that comes up in spoken arithmetic.
///
/// Division forces float arithmetic on the whole expression — we
/// promote all integer literals to their float form so
/// `10 / 4` evaluates to `2.5` (`render_value` then trims the
/// trailing zeros for TTS-friendly output). Without this, evalexpr
/// uses integer division and the user hears "2" for a clearly
/// fractional question.
fn translate_to_expression(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        let replacement = match word {
            // Spanish operators
            "mas" | "más" => "+",
            "menos" => "-",
            "por" | "veces" => "*",
            "entre" | "dividido" => "/",
            // English operators
            "plus" => "+",
            "minus" => "-",
            "times" => "*",
            "divided" => "/",
            // English filler that's safe to drop.
            "by" => "",
            // Spanish single-digit words. v1 keeps it small;
            // larger numbers come through as digits from STT
            // anyway. The map exists so "dos más dos" works
            // when whisper writes it as words.
            "cero" | "zero" => "0",
            "uno" | "una" | "one" => "1",
            "dos" | "two" => "2",
            "tres" | "three" => "3",
            "cuatro" | "four" => "4",
            "cinco" | "five" => "5",
            "seis" | "six" => "6",
            "siete" | "seven" => "7",
            "ocho" | "eight" => "8",
            "nueve" | "nine" => "9",
            "diez" | "ten" => "10",
            other => other,
        };
        if !replacement.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(replacement);
        }
    }
    if out.contains('/') {
        promote_ints_to_floats(&out)
    } else {
        out
    }
}

fn promote_ints_to_floats(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            if !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()) {
                format!("{w}.0")
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_arithmetic(expr: &str) -> bool {
    let has_digit = expr.chars().any(|c| c.is_ascii_digit());
    let has_op = expr.chars().any(|c| "+-*/%^".contains(c));
    has_digit && has_op
}

/// Pretty-print evalexpr's value for TTS. Integer results render
/// as integers ("Eso da 7"); floats trim trailing zeros and round
/// to 4 decimals so we don't read "0.1428571428…" out loud.
fn render_value(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            // Rough rounding to 4 decimals to keep TTS tractable.
            let rounded = (f * 10_000.0).round() / 10_000.0;
            // Drop trailing zeros so "2.0" speaks as "2".
            let formatted = format!("{rounded:.4}");
            let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
            if trimmed.is_empty() {
                "0".to_string()
            } else {
                trimmed.to_string()
            }
        }
        other => other.to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_phrases_match() {
        let h = CalcHandler;
        let session = Session::new();
        for phrase in [
            "cuánto es 2 más 2",
            "cuanto es 5 por 3",
            "calcula 12 entre 4",
            "what is 7 times 6",
            "calculate 10 minus 4",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    /// A trigger word without arithmetic content declines, so the
    /// dispatcher can route the prompt to a real LLM (hija B) or
    /// the default worker.
    #[test]
    fn trigger_without_numbers_declines() {
        let h = CalcHandler;
        let session = Session::new();
        assert!(
            h.recognize("calculate the distance to the moon", &session)
                .is_none()
        );
        assert!(h.recognize("cuanto es la vida", &session).is_none());
    }

    /// Non-trigger prompts decline.
    #[test]
    fn no_trigger_declines() {
        let h = CalcHandler;
        let session = Session::new();
        assert!(h.recognize("hola", &session).is_none());
        assert!(h.recognize("2 más 2", &session).is_none());
    }

    /// `invoke` produces a Spanish-format numeric reply for a
    /// simple expression.
    #[test]
    fn invoke_evaluates_simple_addition() {
        let h = CalcHandler;
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "cuánto es dos más tres",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(resp.text.contains('5'), "got: {:?}", resp.text);
        assert!(resp.text.starts_with("Eso da"));
    }

    /// Word-numbers and word-operators translate correctly. Note
    /// the float-promotion: division forces float arithmetic so
    /// `10 / 4` evaluates to `2.5`, not the integer-truncated `2`.
    /// Non-division expressions stay integer-shaped.
    #[test]
    fn translate_spanish_words() {
        assert_eq!(translate_to_expression("dos mas dos"), "2 + 2");
        assert_eq!(translate_to_expression("cinco por tres"), "5 * 3");
        assert_eq!(translate_to_expression("diez entre cuatro"), "10.0 / 4.0");
    }

    /// Word-numbers and word-operators translate correctly
    /// (English variants).
    #[test]
    fn translate_english_words() {
        assert_eq!(translate_to_expression("seven times six"), "7 * 6");
        assert_eq!(translate_to_expression("ten divided by four"), "10.0 / 4.0");
    }

    /// Division producing a non-integer renders with trimmed
    /// decimals — no "2.0000" or "0.1428571428…".
    #[test]
    fn invoke_renders_clean_decimals() {
        let h = CalcHandler;
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "cuánto es 10 entre 4",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(resp.text.contains("2.5"), "got: {:?}", resp.text);
        // No long trailing decimal string.
        assert!(
            !resp.text.contains("2.50000"),
            "expected trimmed trailing zeros, got: {:?}",
            resp.text
        );
    }

    /// IDs match across traits.
    #[test]
    fn ids_match_across_traits() {
        let h = CalcHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}
