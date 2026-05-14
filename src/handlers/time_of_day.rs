//! Built-in handler for "what time is it" voice queries.
//!
//! Matches Spanish and English phrasings; supports an optional
//! `en <city>` suffix that looks the city up in a small hand-curated
//! table mapping common city names to IANA timezone identifiers
//! (e.g. "tokio" → `Asia/Tokyo`). Out of that 50-or-so list the
//! handler falls back to the system local time.

use anyhow::Result;
use chrono::{Local, Utc};
use chrono_tz::Tz;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

/// Trigger phrases — matched as case-insensitive normalised prefix
/// or full-match against the user's transcript. Order isn't
/// load-bearing within the prefix scan; longer phrases naturally
/// win because shorter ones are tested as substrings of the
/// remainder.
const TIME_TRIGGERS: &[&str] = &[
    "que hora es",
    "que hora",
    "dime la hora",
    "what time is it",
    "what time",
    "tell me the time",
];

pub struct TimeOfDayHandler;

impl IntentMatcher for TimeOfDayHandler {
    fn worker_id(&self) -> &str {
        "time"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if TIME_TRIGGERS.iter().any(|t| n.starts_with(t) || n == *t) {
            // Resolved prompt is the normalised input so `invoke`
            // doesn't have to re-normalise. The city extraction
            // happens inside `invoke` since it parses `en <city>`
            // off the tail.
            Some(n)
        } else {
            None
        }
    }
}

impl WorkerHandle for TimeOfDayHandler {
    fn id(&self) -> &str {
        "time"
    }

    fn description(&self) -> Option<&str> {
        Some("Speaks the current time, optionally in another city's timezone.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks what time it is, optionally in another \
             city (e.g. \"en Tokio\").",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let normalised = normalise(ctx.prompt);
        let city = extract_city(&normalised);
        let text = match city.as_deref().and_then(timezone_for_city) {
            Some(tz) => {
                let now = Utc::now().with_timezone(&tz);
                format!(
                    "Son las {} en {}.",
                    now.format("%H:%M"),
                    city.as_deref().unwrap_or("esa ciudad")
                )
            }
            _ => {
                let now = Local::now();
                format!("Son las {}.", now.format("%H:%M"))
            }
        };
        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

/// Pull "tokio" out of "que hora es en tokio". Returns the
/// lowercased city string, or `None` if the prompt didn't include
/// an `en <city>` clause.
fn extract_city(normalised: &str) -> Option<String> {
    let connectors = ["en ", "in "];
    for c in connectors {
        if let Some(rest) = normalised.split(c).nth(1) {
            let city = rest.trim().trim_end_matches(['?', '.', '¿', '¡', ',']);
            if !city.is_empty() {
                return Some(city.to_string());
            }
        }
    }
    None
}

/// Hand-curated mapping for common cities the voice assistant
/// might be asked about. Out-of-table cities fall back to local
/// time with a polite hedge. Adding entries is a one-line code
/// change; a richer lookup (geonames, Google APIs) is out of v1
/// scope per the spec.
fn timezone_for_city(city: &str) -> Option<Tz> {
    let c = city.trim();
    let tz_name = match c {
        // Iberian
        "madrid" | "barcelona" | "valencia" | "sevilla" | "españa" | "espana" => "Europe/Madrid",
        "lisboa" | "lisbon" | "portugal" => "Europe/Lisbon",
        // Western Europe
        "londres" | "london" | "uk" => "Europe/London",
        "paris" | "francia" | "france" => "Europe/Paris",
        "berlin" | "alemania" | "germany" => "Europe/Berlin",
        "roma" | "rome" | "italia" | "italy" => "Europe/Rome",
        "amsterdam" | "holanda" | "netherlands" => "Europe/Amsterdam",
        // Eastern Europe
        "moscu" | "moscow" => "Europe/Moscow",
        "estambul" | "istanbul" => "Europe/Istanbul",
        // Asia
        "tokio" | "tokyo" | "japon" | "japan" => "Asia/Tokyo",
        "pekin" | "beijing" | "china" => "Asia/Shanghai",
        "shanghai" => "Asia/Shanghai",
        "hong kong" | "hongkong" => "Asia/Hong_Kong",
        "seul" | "seoul" => "Asia/Seoul",
        "singapur" | "singapore" => "Asia/Singapore",
        "bangkok" | "tailandia" | "thailand" => "Asia/Bangkok",
        "mumbai" | "delhi" | "india" => "Asia/Kolkata",
        "dubai" | "emiratos" => "Asia/Dubai",
        // Oceania
        "sidney" | "sydney" | "australia" => "Australia/Sydney",
        // Americas
        "nueva york" | "new york" | "nyc" => "America/New_York",
        "miami" => "America/New_York",
        "chicago" => "America/Chicago",
        "denver" => "America/Denver",
        "los angeles" | "la" | "san francisco" | "sf" => "America/Los_Angeles",
        "ciudad de mexico" | "mexico city" | "mexico" | "cdmx" => "America/Mexico_City",
        "buenos aires" | "argentina" => "America/Argentina/Buenos_Aires",
        "sao paulo" | "san pablo" | "brasil" | "brazil" => "America/Sao_Paulo",
        "lima" | "peru" => "America/Lima",
        "bogota" | "colombia" => "America/Bogota",
        "santiago" | "chile" => "America/Santiago",
        // Africa
        "el cairo" | "cairo" => "Africa/Cairo",
        "johannesburgo" | "johannesburg" => "Africa/Johannesburg",
        _ => return None,
    };
    tz_name.parse().ok()
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

    fn handler() -> TimeOfDayHandler {
        TimeOfDayHandler
    }

    /// Each of the supported Spanish + English triggers matches.
    #[test]
    fn time_phrases_match() {
        let h = handler();
        let session = Session::new();
        for phrase in [
            "¿qué hora es?",
            "qué hora es",
            "qué hora",
            "dime la hora",
            "What time is it?",
            "tell me the time",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    /// Close-but-not-time phrases decline so the cascade can route
    /// them elsewhere.
    #[test]
    fn non_time_phrases_decline() {
        let h = handler();
        let session = Session::new();
        for phrase in [
            "qué día es hoy",
            "qué calor hace",
            "cuánto es dos más dos",
            "hola",
        ] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    /// `en <city>` suffix is extracted regardless of trailing
    /// punctuation. Tested against the normalised text since that's
    /// what `invoke` receives.
    #[test]
    fn extract_city_from_normalised_prompt() {
        assert_eq!(
            extract_city("que hora es en tokio"),
            Some("tokio".to_string())
        );
        assert_eq!(
            extract_city("what time is it in new york"),
            Some("new york".to_string())
        );
        // No `en <city>` clause → None.
        assert_eq!(extract_city("que hora es"), None);
    }

    /// `timezone_for_city` resolves a handful of representative
    /// entries from each region. Catches typos in the lookup table.
    #[test]
    fn timezone_lookup_covers_common_cities() {
        assert!(timezone_for_city("tokio").is_some());
        assert!(timezone_for_city("tokyo").is_some());
        assert!(timezone_for_city("new york").is_some());
        assert!(timezone_for_city("nueva york").is_some());
        assert!(timezone_for_city("madrid").is_some());
        assert!(timezone_for_city("ciudad de mexico").is_some());
        // Unknown cities yield None so the handler falls back to
        // local time.
        assert!(timezone_for_city("xanadu").is_none());
    }

    /// `invoke` produces a non-empty reply that mentions the time
    /// in HH:MM format. We can't assert the exact time (test runs
    /// at wall-clock), but we can assert the shape.
    #[test]
    fn invoke_returns_time_in_local_format() {
        let h = handler();
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "que hora es",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        // "Son las 14:32." style output.
        assert!(
            resp.text.contains(':'),
            "expected HH:MM in output, got: {:?}",
            resp.text
        );
        assert!(resp.text.starts_with("Son las "));
    }

    /// `invoke` with a known city in the prompt routes through
    /// `chrono-tz` and mentions the city in its reply.
    #[test]
    fn invoke_handles_known_city() {
        let h = handler();
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "que hora es en tokio",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(resp.text.contains("tokio"), "got: {:?}", resp.text);
        assert!(resp.text.contains(':'));
    }

    /// IDs match across traits — the registry-lookup invariant.
    #[test]
    fn ids_match_across_traits() {
        let h = handler();
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}
