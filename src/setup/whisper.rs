//! Whisper.cpp model catalog and downloader.
//!
//! whisper.cpp ships *only* the binary; users have to fetch ggml-format
//! models themselves. The official mirror is `huggingface.co/ggerganov/whisper.cpp`.
//! We bundle a small static catalog of the most useful checkpoints (no need
//! to hit the network just to list them) and stream the chosen file to the
//! Jarvis data dir with a progress bar.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::config::data_dir;

/// One entry in the wizard's "pick a model" menu.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// `tiny`, `base`, `small`, `medium`, `large-v3`, …
    pub id: &'static str,
    /// True if the model is English-only (`*.en`). The wizard hides these
    /// unless the user explicitly opts in.
    pub english_only: bool,
    pub size_mb: u32,
    /// Approximate RAM usage during decoding.
    pub ram_mb: u32,
    pub blurb: &'static str,
}

impl ModelInfo {
    pub fn ggml_filename(&self) -> String {
        format!("ggml-{}.bin", self.id)
    }

    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            self.ggml_filename()
        )
    }

    /// Human-friendly label for the select prompt.
    pub fn label(&self) -> String {
        format!(
            "{:<10}  {:>5} MB  ~{:>5} MB RAM   {}",
            self.id, self.size_mb, self.ram_mb, self.blurb
        )
    }
}

/// Curated list — kept short on purpose so the wizard isn't overwhelming.
///
/// Sizes are taken from the upstream README; they shift by a few MB between
/// releases but the order-of-magnitude is what matters for picking.
pub fn catalog() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "tiny",
            english_only: false,
            size_mb: 75,
            ram_mb: 273,
            blurb: "multilingual · fastest, lowest accuracy",
        },
        ModelInfo {
            id: "base",
            english_only: false,
            size_mb: 142,
            ram_mb: 388,
            blurb: "multilingual · balanced (recommended)",
        },
        ModelInfo {
            id: "small",
            english_only: false,
            size_mb: 466,
            ram_mb: 852,
            blurb: "multilingual · better accuracy",
        },
        ModelInfo {
            id: "medium",
            english_only: false,
            size_mb: 1500,
            ram_mb: 2100,
            blurb: "multilingual · high accuracy",
        },
        ModelInfo {
            id: "large-v3",
            english_only: false,
            size_mb: 2950,
            ram_mb: 3900,
            blurb: "multilingual · best accuracy, GPU recommended",
        },
        ModelInfo {
            id: "tiny.en",
            english_only: true,
            size_mb: 75,
            ram_mb: 273,
            blurb: "English-only · ~30% faster than tiny",
        },
        ModelInfo {
            id: "base.en",
            english_only: true,
            size_mb: 142,
            ram_mb: 388,
            blurb: "English-only · ~30% faster than base",
        },
        ModelInfo {
            id: "small.en",
            english_only: true,
            size_mb: 466,
            ram_mb: 852,
            blurb: "English-only · faster small",
        },
    ]
}

/// Where Jarvis stores downloaded whisper models. Honored by the default
/// `[stt].model` after `jarvis setup`.
pub fn models_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("whisper");
    fs::create_dir_all(&d)?;
    Ok(d)
}

/// Already-downloaded file path for a model (whether or not it exists yet).
pub fn local_path(model: &ModelInfo) -> Result<PathBuf> {
    Ok(models_dir()?.join(model.ggml_filename()))
}

/// Download a model with a progress bar. Idempotent — if the destination
/// file already exists with the expected size we skip the network call.
pub fn ensure_downloaded(model: &ModelInfo) -> Result<PathBuf> {
    let dest = local_path(model)?;
    let expected_bytes = (model.size_mb as u64) * 1024 * 1024;
    if let Ok(meta) = fs::metadata(&dest) {
        // Allow ±10% slack against our table — sizes drift slightly between
        // upstream rebuilds; we don't want to redownload 3 GB over 2 MB.
        let slack = expected_bytes / 10;
        if meta.len() > expected_bytes.saturating_sub(slack) {
            return Ok(dest);
        }
    }
    download(model, &dest)?;
    Ok(dest)
}

fn download(model: &ModelInfo, dest: &Path) -> Result<()> {
    let url = model.url();
    let resp = ureq::get(&url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|h| h.parse().ok())
        .unwrap_or((model.size_mb as u64) * 1024 * 1024);

    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    bar.set_message(model.ggml_filename());

    let tmp = dest.with_extension("bin.partial");
    let mut file = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).context("reading from network")?;
        if n == 0 {
            break;
        }
        std::io::copy(&mut &buf[..n], &mut file)?;
        bar.inc(n as u64);
    }
    bar.finish_with_message(format!("downloaded {}", dest.display()));
    fs::rename(&tmp, dest)?;
    Ok(())
}
