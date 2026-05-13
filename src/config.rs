//! Configuration loading.
//!
//! Config lives at `$XDG_CONFIG_HOME/jarvis/config.toml` on Linux/BSD and
//! `~/Library/Application Support/jarvis/config.toml` on macOS. The
//! `directories` crate handles the per-OS conventions so callers never see
//! platform-specific path code.
//!
//! On first run we drop a bundled example next to the resolved path so users
//! always have a self-documenting starting point they can edit.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::Deserialize;

pub const APP_NAME: &str = "jarvis";
pub const ORG: &str = "jarvis";
pub const QUALIFIER: &str = "computer";
pub const CONFIG_FILENAME: &str = "config.toml";

/// Bundled example config — compiled into the binary so a fresh install is
/// always self-sufficient even without /usr/share.
pub const EXAMPLE_CONFIG: &str = include_str!("../config/config.example.toml");

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from(QUALIFIER, ORG, APP_NAME)
        .ok_or_else(|| anyhow!("could not resolve user config directory"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join(CONFIG_FILENAME))
}

pub fn data_dir() -> Result<PathBuf> {
    let p = project_dirs()?.data_dir().to_path_buf();
    fs::create_dir_all(&p).with_context(|| format!("creating data dir: {}", p.display()))?;
    Ok(p)
}

pub fn cache_dir() -> Result<PathBuf> {
    let p = project_dirs()?.cache_dir().to_path_buf();
    fs::create_dir_all(&p).with_context(|| format!("creating cache dir: {}", p.display()))?;
    Ok(p)
}

/// Ensure the config file exists at `config_path()` and return that path.
pub fn ensure_config() -> Result<PathBuf> {
    let path = config_path()?;
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir: {}", parent.display()))?;
    }
    fs::write(&path, EXAMPLE_CONFIG)
        .with_context(|| format!("writing initial config: {}", path.display()))?;
    Ok(path)
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct WakeConfig {
    pub enabled: bool,
    pub model: String,
    pub threshold: f32,
    pub cooldown_seconds: f32,
    pub sample_rate: u32,
    pub input_device: Option<String>,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "hey_jarvis".into(),
            threshold: 0.5,
            cooldown_seconds: 2.0,
            sample_rate: 16_000,
            input_device: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct RecordConfig {
    /// `auto` lets jarvis pick the first available recorder.
    pub backend: String,
    pub device: Option<String>,
    pub sample_rate: u32,
    pub channels: u32,
    pub max_seconds: f32,
    pub silence_seconds: f32,
    /// Used only when `backend = "command"`. `{out}` is replaced with the WAV path.
    pub command: Vec<String>,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            device: None,
            sample_rate: 16_000,
            channels: 1,
            max_seconds: 15.0,
            silence_seconds: 1.5,
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    pub backend: String,
    pub binary: String,
    pub model: String,
    pub language: String,
    pub threads: u32,
    pub extra_args: Vec<String>,
    /// Used only when `backend = "command"`. `{wav}` is replaced with the WAV path.
    pub command: Vec<String>,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            backend: "whisper-cli".into(),
            binary: "whisper-cli".into(),
            model: "/usr/share/whisper.cpp/models/ggml-base.en.bin".into(),
            language: "en".into(),
            threads: 4,
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    pub backend: String,
    pub voice: String,
    pub piper_binary: String,
    pub piper_model_path: Option<String>,
    pub espeak_voice: String,
    pub rate: u32,
    /// Used only when `backend = "command"`. `{text}` is replaced with the response text.
    pub command: Vec<String>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            backend: "piper".into(),
            voice: "en_US-lessac-medium".into(),
            piper_binary: "piper".into(),
            piper_model_path: None,
            espeak_voice: "en".into(),
            rate: 175,
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AgentConfig {
    pub name: String,
    /// Backend-specific options. Anything other than `name` is forwarded to
    /// the agent constructor as-is.
    #[serde(flatten)]
    pub options: toml::Table,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "claude".into(),
            options: toml::Table::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct JarvisConfig {
    pub log_level: String,
    pub speak_responses: bool,
    pub wake: WakeConfig,
    pub record: RecordConfig,
    pub stt: SttConfig,
    pub tts: TtsConfig,
    pub agent: AgentConfig,
}

impl Default for JarvisConfig {
    fn default() -> Self {
        Self {
            log_level: "INFO".into(),
            speak_responses: true,
            wake: WakeConfig::default(),
            record: RecordConfig::default(),
            stt: SttConfig::default(),
            tts: TtsConfig::default(),
            agent: AgentConfig::default(),
        }
    }
}

pub fn load(path: &Path) -> Result<JarvisConfig> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("reading config: {}", path.display()))?;
    let mut cfg: JarvisConfig =
        toml::from_str(&raw).with_context(|| format!("parsing config: {}", path.display()))?;

    // `JARVIS_AGENT=openai jarvis listen` overrides the configured agent
    // without editing the file. Handy for testing.
    if let Ok(agent) = std::env::var("JARVIS_AGENT") {
        cfg.agent.name = agent;
    }
    Ok(cfg)
}
