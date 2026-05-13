//! Voice intent recognition for spec management.
//!
//! Detection is deliberately **deterministic and stupid**: prefix matching
//! against an explicit phrase list, no regex, no LLM round-trip. The trade-
//! off is that the user has to learn the phrases, but in return:
//!
//!   - Recognition is O(phrases); cost is invisible vs. the wake/STT/agent
//!     latency around it.
//!   - The phrase list is the spec — `grep` to find what's accepted.
//!   - Adding a language (Italian, French) is a config edit, not a
//!     classifier retrain.
//!
//! We normalise the prompt the same way the wake module does (lowercase,
//! strip accents and punctuation, collapse whitespace) before matching.

/// What the user wants us to do, decoded from a transcribed prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// "Open a spec for streaming TTS" → create inbox with title.
    NewSpec { title: String },
    /// "List specs" / "show me the specs" → summary by status.
    ListSpecs,
    /// "Show spec 14" → read aloud the title + acceptance criteria.
    ShowSpec { query: String },
    /// "Promote spec 14" / "promote streaming-tts".
    PromoteSpec { query: String },
    /// "Ship spec 14".
    ShipSpec { query: String },
    /// "Reject spec 14 because <reason>".
    RejectSpec { query: String, reason: String },
}

/// Return `Some(intent)` if `prompt` should be handled by the specs
/// subsystem instead of forwarded to the agent. `None` lets the prompt
/// proceed to the agent as usual.
pub fn recognize(prompt: &str) -> Option<Intent> {
    let n = normalise(prompt);
    if n.is_empty() {
        return None;
    }

    // `new` — order matters: longer prefixes first so "open a new spec for"
    // doesn't get half-matched by "new spec".
    for prefix in [
        "abre un spec para ",
        "abre un spec ",
        "crea un spec para ",
        "crea un spec ",
        "nuevo spec para ",
        "nuevo spec ",
        "open a spec for ",
        "open a spec ",
        "new spec for ",
        "new spec ",
        "create a spec for ",
        "create a spec ",
    ] {
        if let Some(rest) = n.strip_prefix(prefix)
            && !rest.trim().is_empty()
        {
            return Some(Intent::NewSpec {
                title: rest.trim().to_string(),
            });
        }
    }

    // `list` — full-utterance equality. Otherwise "list everything you
    // know about specs" would trip the matcher.
    for whole in [
        "lista los specs",
        "lista las specs",
        "lista specs",
        "list specs",
        "list the specs",
        "show me the specs",
    ] {
        if n == whole {
            return Some(Intent::ListSpecs);
        }
    }

    // `show <q>`
    for prefix in [
        "muestra el spec ",
        "muestra spec ",
        "muestrame el spec ",
        "muestrame spec ",
        "show spec ",
        "show me spec ",
        "show the spec ",
    ] {
        if let Some(rest) = n.strip_prefix(prefix)
            && !rest.trim().is_empty()
        {
            return Some(Intent::ShowSpec {
                query: rest.trim().to_string(),
            });
        }
    }

    // `promote`
    for prefix in [
        "promueve el spec ",
        "promueve spec ",
        "promover spec ",
        "promote spec ",
        "promote the spec ",
    ] {
        if let Some(rest) = n.strip_prefix(prefix)
            && !rest.trim().is_empty()
        {
            return Some(Intent::PromoteSpec {
                query: rest.trim().to_string(),
            });
        }
    }

    // `ship`
    for prefix in [
        "lanza el spec ",
        "lanza spec ",
        "envia el spec ",
        "envia spec ",
        "marca como hecho el spec ",
        "ship spec ",
        "ship the spec ",
    ] {
        if let Some(rest) = n.strip_prefix(prefix)
            && !rest.trim().is_empty()
        {
            return Some(Intent::ShipSpec {
                query: rest.trim().to_string(),
            });
        }
    }

    // `reject` — captures both id and reason. The reason starts after the
    // first `porque` / `because` keyword, if any; otherwise the rest of
    // the utterance is the reason.
    for prefix in [
        "rechaza el spec ",
        "rechaza spec ",
        "reject spec ",
        "reject the spec ",
    ] {
        if let Some(rest) = n.strip_prefix(prefix) {
            let rest = rest.trim();
            if rest.is_empty() {
                continue;
            }
            let (query, reason) = split_reason(rest);
            return Some(Intent::RejectSpec {
                query: query.to_string(),
                reason: reason.to_string(),
            });
        }
    }

    None
}

fn split_reason(rest: &str) -> (&str, &str) {
    for sep in [" porque ", " because ", " razon ", " reason "] {
        if let Some(pos) = rest.find(sep) {
            return (rest[..pos].trim(), rest[pos + sep.len()..].trim());
        }
    }
    // No explicit "because" keyword: first whitespace-delimited token is
    // the query, rest is the reason. Falls back gracefully when the user
    // only gives an ID and no reason.
    match rest.split_once(' ') {
        Some((q, r)) => (q.trim(), r.trim()),
        None => (rest.trim(), ""),
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
    fn recognises_new_spec_phrases() {
        let cases = [
            "Abre un spec para streaming TTS",
            "crea un spec para streaming TTS",
            "Open a spec for streaming TTS",
            "new spec streaming TTS",
        ];
        for c in cases {
            let intent = recognize(c).expect(c);
            assert!(matches!(intent, Intent::NewSpec { ref title } if title.contains("streaming")));
        }
    }

    #[test]
    fn recognises_list_only_on_exact_match() {
        assert_eq!(recognize("list specs"), Some(Intent::ListSpecs));
        assert_eq!(recognize("lista los specs"), Some(Intent::ListSpecs));
        // Substring match must NOT fire.
        assert!(recognize("list all my favourite specs please").is_none());
    }

    #[test]
    fn recognises_show_with_id() {
        let i = recognize("muestra el spec 14").unwrap();
        assert_eq!(
            i,
            Intent::ShowSpec {
                query: "14".to_string()
            }
        );
    }

    #[test]
    fn recognises_reject_with_reason() {
        let i = recognize("rechaza el spec 14 porque no escala").unwrap();
        assert_eq!(
            i,
            Intent::RejectSpec {
                query: "14".to_string(),
                reason: "no escala".to_string()
            }
        );
    }

    #[test]
    fn ignores_unrelated_prompts() {
        assert!(recognize("qué hora es en Tokio").is_none());
        assert!(recognize("explícame el código de stt.rs").is_none());
        // Bare "spec" without a verb shouldn't trip anything.
        assert!(recognize("specs").is_none());
    }
}
