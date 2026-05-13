//! Piper voice catalog.
//!
//! rhasspy/piper-voices publishes a `voices.json` index covering every
//! checkpoint they host. We fetch it once at wizard time, filter by the
//! detected language, and present the result as a select menu. We avoid
//! bundling a static copy — that catalog grows every few weeks and a stale
//! list would just confuse users.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

const VOICES_INDEX_URL: &str = "https://huggingface.co/rhasspy/piper-voices/raw/main/voices.json";

/// One voice entry from the upstream `voices.json`. Only the fields the
/// wizard uses are kept — Piper ships a lot of metadata we don't need.
#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    /// Voice ID we hand back to the user, e.g. `es_ES-davefx-medium`.
    pub key: String,
    pub language: VoiceLanguage,
    /// Speaker display name. Kept for richer future labels even though the
    /// minimal MVP doesn't surface it.
    #[allow(dead_code)]
    pub name: String,
    /// `x_low`, `low`, `medium`, `high`.
    pub quality: String,
    #[serde(default)]
    pub num_speakers: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceLanguage {
    /// Two-letter family, e.g. `es`.
    pub family: String,
    /// `<lang>_<REGION>`, e.g. `es_ES`.
    pub code: String,
    #[serde(rename = "name_native")]
    #[allow(dead_code)] // surfaced when we add a per-language pretty header
    pub name_native: Option<String>,
}

impl Voice {
    /// Friendly menu label.
    pub fn label(&self) -> String {
        let speakers = match self.num_speakers {
            Some(n) if n > 1 => format!(" · {n} speakers"),
            _ => String::new(),
        };
        format!(
            "{:<32}  ({}, {}{})",
            self.key, self.language.code, self.quality, speakers
        )
    }
}

/// Download the upstream voice index and return a parsed list.
///
/// The index is a JSON object keyed by voice ID rather than an array, so we
/// deserialise into a `BTreeMap` and flatten to a `Vec` so callers can sort
/// / filter ergonomically.
pub fn fetch_index() -> Result<Vec<Voice>> {
    let resp = ureq::get(VOICES_INDEX_URL)
        .call()
        .with_context(|| format!("GET {VOICES_INDEX_URL}"))?;
    let map: BTreeMap<String, Voice> = resp.into_json().context("decoding piper voices.json")?;
    Ok(map.into_values().collect())
}

/// Return voices whose language family matches `lang` (e.g. `es`), sorted
/// alphabetically by ID for stable display order. Falls back to the full
/// list if nothing matches.
pub fn filter_by_language(all: Vec<Voice>, lang: &str) -> Vec<Voice> {
    let mut filtered: Vec<Voice> = all
        .iter()
        .filter(|v| v.language.family.eq_ignore_ascii_case(lang))
        .cloned()
        .collect();
    if filtered.is_empty() {
        filtered = all;
    }
    filtered.sort_by(|a, b| a.key.cmp(&b.key));
    filtered
}
