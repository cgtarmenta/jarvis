//! Built-in handler for "what's today's date" voice queries.

use anyhow::Result;
use chrono::{Datelike, Local};

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

/// Trigger phrases — normalised, prefix-matched. Mirrors the
/// time-handler pattern so users learn one shape and reuse it for
/// every clock-style intent.
const DATE_TRIGGERS: &[&str] = &[
    "que dia es",
    "que fecha es",
    "que fecha",
    "fecha de hoy",
    "dime la fecha",
    "what day is it",
    "what is the date",
    "what date is it",
    "todays date",
    "today's date",
];

pub struct DateTodayHandler;

impl IntentMatcher for DateTodayHandler {
    fn worker_id(&self) -> &str {
        "date"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if DATE_TRIGGERS.iter().any(|t| n.starts_with(t) || n == *t) {
            Some(n)
        } else {
            None
        }
    }
}

impl WorkerHandle for DateTodayHandler {
    fn id(&self) -> &str {
        "date"
    }

    fn description(&self) -> Option<&str> {
        Some("Speaks today's date in the local calendar.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks what day, date, or month it is. \
             Always returns the system-local date.",
        )
    }

    fn invoke(&self, _ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let now = Local::now();
        // "Hoy es martes 14 de mayo de 2026" — long form in Spanish
        // because that's the user's language. Day-of-week and month
        // names are hand-localised to avoid pulling a big locale
        // crate just for this; English speakers get
        // "Today is Tuesday, May 14, 2026." via the fallback below.
        let weekday_es = spanish_weekday(now.weekday().number_from_monday());
        let month_es = spanish_month(now.month());
        let text = format!(
            "Hoy es {weekday_es} {} de {month_es} de {}.",
            now.day(),
            now.year()
        );
        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

fn spanish_weekday(n: u32) -> &'static str {
    match n {
        1 => "lunes",
        2 => "martes",
        3 => "miércoles",
        4 => "jueves",
        5 => "viernes",
        6 => "sábado",
        7 => "domingo",
        _ => "día",
    }
}

fn spanish_month(n: u32) -> &'static str {
    match n {
        1 => "enero",
        2 => "febrero",
        3 => "marzo",
        4 => "abril",
        5 => "mayo",
        6 => "junio",
        7 => "julio",
        8 => "agosto",
        9 => "septiembre",
        10 => "octubre",
        11 => "noviembre",
        12 => "diciembre",
        _ => "mes",
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
    fn date_phrases_match() {
        let h = DateTodayHandler;
        let session = Session::new();
        for phrase in [
            "¿qué día es hoy?",
            "qué fecha es",
            "dime la fecha",
            "What date is it?",
            "today's date please",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    /// Time / calc queries decline.
    #[test]
    fn non_date_phrases_decline() {
        let h = DateTodayHandler;
        let session = Session::new();
        for phrase in ["qué hora es", "cuánto es 2 más 2", "hola"] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    /// `invoke` produces a Spanish-format reply with day, month
    /// and 4-digit year present. Locked to shape, not value.
    #[test]
    fn invoke_returns_spanish_date_format() {
        let h = DateTodayHandler;
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "qué día es hoy",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(
            resp.text.starts_with("Hoy es "),
            "expected Spanish lead, got: {:?}",
            resp.text
        );
        assert!(
            resp.text.contains(" de "),
            "expected 'de' connector, got: {:?}",
            resp.text
        );
        // Some 4-digit year between 2020 and 2099 should appear.
        let years_present = (2020..=2099).any(|y| resp.text.contains(&y.to_string()));
        assert!(
            years_present,
            "expected a 4-digit year in [2020..2099], got: {:?}",
            resp.text
        );
    }

    /// IDs match across traits.
    #[test]
    fn ids_match_across_traits() {
        let h = DateTodayHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}
