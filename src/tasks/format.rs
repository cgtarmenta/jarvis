//! Small formatting helpers shared between the `jarvis task`
//! CLI surface and the voice handlers (spec 0012 / E2).

/// Short relative-age string ("5s", "3m", "2h", "1d 4h"). The
/// CLI list view, the detail view, and the voice intent that
/// announces "está corriendo desde hace 3 minutos" all want the
/// same shape — keeping it in one place means a future tweak
/// affects every surface consistently.
pub fn humanise_age(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86_400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            return format!("{h}h");
        }
        return format!("{h}h {m}m");
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    if h == 0 {
        format!("{d}d")
    } else {
        format!("{d}d {h}h")
    }
}

/// TTS-friendly version of `humanise_age`: spells the unit out
/// in Spanish ("3 minutos", "1 día") so the listener doesn't
/// say "uno-de hache" out loud. Single-second / single-minute
/// forms use the singular noun.
pub fn humanise_age_spanish(secs: u64) -> String {
    if secs < 60 {
        let s = if secs == 1 { "segundo" } else { "segundos" };
        return format!("{secs} {s}");
    }
    if secs < 3600 {
        let m = secs / 60;
        let label = if m == 1 { "minuto" } else { "minutos" };
        return format!("{m} {label}");
    }
    if secs < 86_400 {
        let h = secs / 3600;
        let label = if h == 1 { "hora" } else { "horas" };
        return format!("{h} {label}");
    }
    let d = secs / 86_400;
    let label = if d == 1 { "día" } else { "días" };
    format!("{d} {label}")
}

/// Truncate a string at `max_chars` codepoints with a trailing
/// `…` when the cut was lossy. Used for one-line summaries of
/// task `user_intent` strings in list views.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max_chars).collect();
    format!("{prefix}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanise_age_brackets() {
        assert_eq!(humanise_age(0), "0s");
        assert_eq!(humanise_age(45), "45s");
        assert_eq!(humanise_age(60), "1m");
        assert_eq!(humanise_age(120), "2m");
        assert_eq!(humanise_age(3600), "1h");
        assert_eq!(humanise_age(3660), "1h 1m");
        assert_eq!(humanise_age(86_400), "1d");
        assert_eq!(humanise_age(90_000), "1d 1h");
    }

    /// Singulars / plurals on the Spanish-spoken form.
    #[test]
    fn humanise_age_spanish_plurality() {
        assert_eq!(humanise_age_spanish(1), "1 segundo");
        assert_eq!(humanise_age_spanish(30), "30 segundos");
        assert_eq!(humanise_age_spanish(60), "1 minuto");
        assert_eq!(humanise_age_spanish(120), "2 minutos");
        assert_eq!(humanise_age_spanish(3600), "1 hora");
        assert_eq!(humanise_age_spanish(7200), "2 horas");
        assert_eq!(humanise_age_spanish(86_400), "1 día");
        assert_eq!(humanise_age_spanish(172_800), "2 días");
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_adds_ellipsis_when_cut() {
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        // UTF-8: 4 characters, length in bytes is 6.
        assert_eq!(truncate_chars("café✓", 3), "caf…");
    }
}
