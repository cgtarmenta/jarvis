//! Text-to-speech: subprocess wrappers for piper / espeak-ng / arbitrary CLIs.
//!
//! Piper is the recommended default — it's CPU-friendly neural TTS that
//! sounds dramatically better than espeak-ng and ships as a single static
//! binary on Linux and macOS. Voices auto-download to the user's data dir
//! on first use; subsequent runs hit local cache.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use tempfile::NamedTempFile;
use tracing::{info, warn};

use crate::config::{TtsConfig, data_dir};
use crate::recorder::play_wav;

const PIPER_VOICE_BASE: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main";

pub trait Tts {
    fn speak(&self, text: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Piper
// ---------------------------------------------------------------------------

fn piper_voice_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("piper");
    fs::create_dir_all(&d)?;
    Ok(d)
}

/// Parse a piper voice id like `en_US-lessac-medium` into its components.
/// Returns `(lang_full, name, quality)`. The HuggingFace voice layout is:
///   `<family>/<lang_full>/<name>/<quality>/<voice>.onnx`
fn parse_voice(voice: &str) -> Result<(String, String, String)> {
    let parts: Vec<&str> = voice.splitn(3, '-').collect();
    if parts.len() != 3 {
        return Err(anyhow!("voice id must look like en_US-lessac-medium"));
    }
    Ok((parts[0].into(), parts[1].into(), parts[2].into()))
}

fn download(url: &str, dest: &Path) -> Result<()> {
    info!(url, "downloading piper voice file");
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_reader();
    let tmp_path = dest.with_extension(format!(
        "{}.tmp",
        dest.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    let mut file = fs::File::create(&tmp_path)?;
    std::io::copy(&mut reader, &mut file)?;
    file.flush()?;
    fs::rename(&tmp_path, dest)?;
    Ok(())
}

fn ensure_voice(voice: &str) -> Result<PathBuf> {
    let dir = piper_voice_dir()?;
    let onnx = dir.join(format!("{voice}.onnx"));
    let json = dir.join(format!("{voice}.onnx.json"));
    if onnx.is_file() && json.is_file() {
        return Ok(onnx);
    }
    let (lang_full, name, quality) = parse_voice(voice)?;
    let family = lang_full.split('_').next().unwrap_or("en");
    let base = format!("{PIPER_VOICE_BASE}/{family}/{lang_full}/{name}/{quality}");
    download(&format!("{base}/{voice}.onnx"), &onnx)?;
    download(&format!("{base}/{voice}.onnx.json"), &json)?;
    Ok(onnx)
}

pub struct Piper {
    cfg: TtsConfig,
    model: PathBuf,
}

impl Piper {
    pub fn new(cfg: TtsConfig) -> Result<Self> {
        if which::which(&cfg.piper_binary).is_err() {
            return Err(anyhow!(
                "piper binary {:?} not found in PATH",
                cfg.piper_binary
            ));
        }
        let model = if let Some(path) = &cfg.piper_model_path {
            PathBuf::from(path)
        } else {
            ensure_voice(&cfg.voice)?
        };
        Ok(Self { cfg, model })
    }
}

impl Tts for Piper {
    fn speak(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let tmp = NamedTempFile::with_prefix("jarvis-tts-")?;
        let (_, wav_path) = tmp.keep().context("persisting tts wav")?;
        let result = {
            let mut child = Command::new(&self.cfg.piper_binary)
                .args(["--model"])
                .arg(&self.model)
                .args(["--output_file"])
                .arg(&wav_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("spawning {}", self.cfg.piper_binary))?;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| anyhow!("piper stdin unavailable"))?
                .write_all(text.as_bytes())?;
            let out = child.wait_with_output()?;
            if !out.status.success() {
                Err(anyhow!(
                    "piper exited with {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            } else {
                play_wav(&wav_path)
            }
        };
        let _ = fs::remove_file(&wav_path);
        result
    }
}

// ---------------------------------------------------------------------------
// espeak-ng
// ---------------------------------------------------------------------------

pub struct Espeak {
    binary: String,
    cfg: TtsConfig,
}

impl Espeak {
    pub fn new(cfg: TtsConfig) -> Result<Self> {
        let binary = if which::which("espeak-ng").is_ok() {
            "espeak-ng".to_string()
        } else if which::which("espeak").is_ok() {
            "espeak".to_string()
        } else {
            return Err(anyhow!("espeak-ng not installed"));
        };
        Ok(Self { binary, cfg })
    }
}

impl Tts for Espeak {
    fn speak(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let _ = Command::new(&self.binary)
            .args([
                "-v",
                &self.cfg.espeak_voice,
                "-s",
                &self.cfg.rate.to_string(),
            ])
            .arg(text)
            .status();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Generic command + noop
// ---------------------------------------------------------------------------

pub struct CommandTts {
    cfg: TtsConfig,
}

impl CommandTts {
    pub fn new(cfg: TtsConfig) -> Result<Self> {
        if cfg.command.is_empty() {
            return Err(anyhow!("tts.backend=\"command\" requires tts.command"));
        }
        Ok(Self { cfg })
    }
}

impl Tts for CommandTts {
    fn speak(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let argv: Vec<String> = self
            .cfg
            .command
            .iter()
            .map(|a| a.replace("{text}", text))
            .collect();
        Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .with_context(|| format!("spawning {}", argv[0]))?;
        Ok(())
    }
}

pub struct NoopTts;

impl Tts for NoopTts {
    fn speak(&self, text: &str) -> Result<()> {
        info!("[TTS disabled] would speak: {text}");
        Ok(())
    }
}

pub fn build(cfg: TtsConfig) -> Result<Box<dyn Tts + Send + Sync>> {
    match cfg.backend.to_lowercase().as_str() {
        "none" | "off" | "disabled" => Ok(Box::new(NoopTts)),
        "piper" => match Piper::new(cfg.clone()) {
            Ok(p) => Ok(Box::new(p)),
            Err(e) => {
                warn!("piper unavailable ({e}) — falling back to espeak-ng");
                Ok(Box::new(Espeak::new(cfg)?))
            }
        },
        "espeak" | "espeak-ng" => Ok(Box::new(Espeak::new(cfg)?)),
        "command" => Ok(Box::new(CommandTts::new(cfg)?)),
        other => Err(anyhow!("unsupported TTS backend: {other:?}")),
    }
}
