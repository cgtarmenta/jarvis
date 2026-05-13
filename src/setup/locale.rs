//! Locale detection and language-to-defaults mapping.
//!
//! We read `$LANG` (then `$LC_ALL`, `$LC_MESSAGES`) the same way every POSIX
//! tool does, parse out the `lang_REGION` portion, and translate that into
//! sensible defaults for Whisper and Piper.
//!
//! Two rules guide the mapping:
//! - **English defaults to UK** (`en-GB`) when only a generic `en` is set
//!   or the region isn't one we explicitly handle. Neutral-ish, slightly
//!   less weighted than the US default in the Whisper/Piper ecosystem.
//! - **Spanish defaults to Spain** (`es-ES`) on the same rationale. This is
//!   a deliberate choice: tools default to US/MX too often, and a European
//!   contributor base is the project's primary audience.

use std::env;

/// A locale we know how to turn into Jarvis defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    /// Two-letter ISO-639 code, lowercase (e.g. `en`, `es`, `fr`).
    pub lang: String,
    /// Two-letter ISO-3166 region, uppercase (e.g. `GB`, `ES`, `MX`).
    /// Empty when the source locale gave us only the language.
    pub region: String,
}

impl Locale {
    /// The `lang_REGION` form used by Piper voice IDs (`es_ES`, `en_GB`).
    /// Kept on the public API for future callers (e.g. a TUI status panel)
    /// even though the wizard goes through `Defaults` today.
    #[allow(dead_code)]
    pub fn piper_lang(&self) -> String {
        if self.region.is_empty() {
            self.lang.clone()
        } else {
            format!("{}_{}", self.lang, self.region)
        }
    }

    /// The two-letter Whisper language code (`es`, `en`, …).
    #[allow(dead_code)]
    pub fn whisper_code(&self) -> &str {
        &self.lang
    }

    /// Friendly display string, e.g. "Spanish (Spain)".
    pub fn pretty(&self) -> String {
        let lang = match self.lang.as_str() {
            "en" => "English",
            "es" => "Spanish",
            "fr" => "French",
            "de" => "German",
            "it" => "Italian",
            "pt" => "Portuguese",
            "nl" => "Dutch",
            "ja" => "Japanese",
            "zh" => "Chinese",
            "ru" => "Russian",
            other => return format!("{other} ({})", self.region),
        };
        let region = match (self.lang.as_str(), self.region.as_str()) {
            ("en", "GB") => "United Kingdom",
            ("en", "US") => "United States",
            ("en", "AU") => "Australia",
            ("en", "IE") => "Ireland",
            ("es", "ES") => "Spain",
            ("es", "MX") => "Mexico",
            ("es", "AR") => "Argentina",
            ("pt", "BR") => "Brazil",
            ("pt", "PT") => "Portugal",
            ("zh", "CN") => "Mainland China",
            ("zh", "TW") => "Taiwan",
            (_, "") => return lang.to_string(),
            (_, r) => r,
        };
        format!("{lang} ({region})")
    }
}

/// Read the system locale from environment variables, falling back to a
/// neutral default if nothing is set.
pub fn detect() -> Locale {
    // `LC_ALL` overrides everything; otherwise `LC_MESSAGES` controls UI
    // language; `LANG` is the catch-all fallback. Matches POSIX semantics.
    let raw = env::var("LC_ALL")
        .ok()
        .filter(|s| !s.is_empty() && s != "C" && s != "POSIX")
        .or_else(|| env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()))
        .or_else(|| env::var("LANG").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "en_GB.UTF-8".to_string());
    parse(&raw).unwrap_or(default_fallback())
}

fn parse(raw: &str) -> Option<Locale> {
    // Strip codeset (`.UTF-8`) and modifier (`@euro`); we only care about the
    // `lang_REGION` chunk.
    let head = raw.split('.').next().unwrap_or(raw);
    let head = head.split('@').next().unwrap_or(head);
    if head.is_empty() || head == "C" || head == "POSIX" {
        return None;
    }
    let mut parts = head.splitn(2, &['_', '-'][..]);
    let lang = parts.next()?.to_lowercase();
    let region = parts.next().unwrap_or("").to_uppercase();
    Some(Locale { lang, region })
}

/// `en_GB` — chosen deliberately over `en_US`. See module docs.
fn default_fallback() -> Locale {
    Locale {
        lang: "en".into(),
        region: "GB".into(),
    }
}

/// Defaults we propose in the wizard before the user picks anything.
#[derive(Debug, Clone)]
pub struct Defaults {
    /// Whisper language code (`es`, `en`, …).
    pub whisper_language: String,
    /// Whisper ggml model id (multilingual unless we're explicitly english-only).
    /// Surfaced as a *hint* in the wizard; the user picks from the catalog.
    #[allow(dead_code)]
    pub whisper_model: &'static str,
    /// Piper voice ID (e.g. `es_ES-davefx-medium`).
    pub piper_voice: String,
    /// Piper language filter for the voice picker (`es`, `en`).
    pub piper_lang_filter: String,
}

/// Pick sensible defaults for a detected locale.
///
/// **Whisper:** we *don't* default to the english-only `*.en` models. They're
/// ~30% faster but only work for one language; the multilingual `base` covers
/// every user. Speed-conscious english-only users can switch in the wizard.
///
/// **Piper:** voice IDs come from the rhasspy/piper-voices repo. We hardcode
/// one popular voice per language as the starting suggestion; the wizard
/// fetches the full list at runtime so the user can pick another.
pub fn defaults_for(locale: &Locale) -> Defaults {
    // Translate to the region we'll *propose* by default — collapsing
    // unrecognised English regions to GB and unrecognised Spanish to ES.
    let (lang, region) = match (locale.lang.as_str(), locale.region.as_str()) {
        ("en", region) if matches!(region, "GB" | "US" | "AU" | "IE" | "CA" | "ZA") => {
            ("en", region)
        }
        ("en", _) => ("en", "GB"),
        ("es", region) if matches!(region, "ES" | "MX" | "AR" | "US") => ("es", region),
        ("es", _) => ("es", "ES"),
        (other, region) => (other, region),
    };

    // Piper voice ID per (lang, region). These are voices that exist in the
    // rhasspy/piper-voices catalog as of writing; the wizard will offer the
    // full live list, so a stale entry here only means a worse first pick.
    let piper_voice = match (lang, region) {
        ("en", "GB") => "en_GB-alan-medium",
        ("en", "US") => "en_US-lessac-medium",
        ("en", "AU") => "en_GB-alan-medium", // no AU voice; closest neighbour
        ("en", _) => "en_GB-alan-medium",
        ("es", "ES") => "es_ES-davefx-medium",
        ("es", "MX") => "es_MX-claude-high",
        ("es", "AR") => "es_AR-daniela-high",
        ("es", _) => "es_ES-davefx-medium",
        ("fr", _) => "fr_FR-siwis-medium",
        ("de", _) => "de_DE-thorsten-medium",
        ("it", _) => "it_IT-paola-medium",
        ("pt", "BR") => "pt_BR-faber-medium",
        ("pt", _) => "pt_PT-tugão-medium",
        ("nl", _) => "nl_NL-mls-medium",
        // For anything unmapped, fall through to en_GB so the wizard still
        // boots; the user picks their real voice in the voice step.
        (_, _) => "en_GB-alan-medium",
    };

    Defaults {
        whisper_language: lang.to_string(),
        whisper_model: "base", // multilingual
        piper_voice: piper_voice.to_string(),
        piper_lang_filter: lang.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_es_es() {
        let loc = parse("es_ES.UTF-8").unwrap();
        assert_eq!(loc.lang, "es");
        assert_eq!(loc.region, "ES");
        assert_eq!(loc.piper_lang(), "es_ES");
    }

    #[test]
    fn parses_with_modifier() {
        let loc = parse("ca_ES@valencia.UTF-8").unwrap();
        assert_eq!(loc.lang, "ca");
        assert_eq!(loc.region, "ES");
    }

    #[test]
    fn parses_lang_only() {
        let loc = parse("en").unwrap();
        assert_eq!(loc.lang, "en");
        assert!(loc.region.is_empty());
    }

    #[test]
    fn c_locale_rejected() {
        assert!(parse("C").is_none());
        assert!(parse("POSIX").is_none());
    }

    #[test]
    fn english_unknown_region_falls_back_to_gb() {
        let d = defaults_for(&Locale {
            lang: "en".into(),
            region: "NZ".into(),
        });
        assert_eq!(d.piper_voice, "en_GB-alan-medium");
    }

    #[test]
    fn spanish_unknown_region_falls_back_to_es() {
        let d = defaults_for(&Locale {
            lang: "es".into(),
            region: "GT".into(),
        });
        assert_eq!(d.piper_voice, "es_ES-davefx-medium");
    }

    #[test]
    fn spanish_spain() {
        let d = defaults_for(&Locale {
            lang: "es".into(),
            region: "ES".into(),
        });
        assert_eq!(d.whisper_language, "es");
        assert_eq!(d.whisper_model, "base");
        assert_eq!(d.piper_voice, "es_ES-davefx-medium");
    }

    #[test]
    fn pretty_names() {
        let loc = Locale {
            lang: "es".into(),
            region: "ES".into(),
        };
        assert_eq!(loc.pretty(), "Spanish (Spain)");
    }
}
