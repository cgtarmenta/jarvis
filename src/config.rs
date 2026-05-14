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
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "jarvis";
pub const ORG: &str = "jarvis";
pub const QUALIFIER: &str = "computer";
pub const CONFIG_FILENAME: &str = "config.toml";

/// Schema version of the TOML config. Bump this whenever a breaking change
/// is made: field removed, field renamed, semantics changed. Adding new
/// fields with sensible defaults does **not** require a bump.
///
/// Old user configs with a lower `config_version` (or no version at all)
/// fail to load with a clear message pointing them at `jarvis setup`.
///
/// Changelog:
///   1 — initial Rust release (implicit; configs without `config_version`)
///   2 — `[wake]` schema rewrite: `model` removed, `backend` + `phrases` added.
///       Wake-word backend made pluggable.
pub const CURRENT_CONFIG_VERSION: u32 = 2;

/// Bundled example config — compiled into the binary so a fresh install is
/// always self-sufficient even without /usr/share.
pub const EXAMPLE_CONFIG: &str = include_str!("../config/config.example.toml");

/// Bundled starter manifest for the `claude` worker. Dropped into
/// `~/.config/jarvis/workers/claude.toml` by [`ensure_workers_dir`] on
/// first run so the registry has a working default without forcing the
/// user through `jarvis setup` again. See spec 0008 (orchestrator C)
/// for context.
pub const STARTER_CLAUDE_MANIFEST: &str = include_str!("../config/workers/claude.toml");

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

/// Path to the workers manifest directory:
/// `~/.config/jarvis/workers/` on Linux/BSD, the platform equivalent on
/// macOS. Does not create the directory — see [`ensure_workers_dir`] for
/// that. Spec 0008 (orchestrator C) makes this the single autodiscovery
/// path for worker manifests.
pub fn workers_dir() -> Result<PathBuf> {
    let cfg = config_path()?;
    let parent = cfg
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", cfg.display()))?;
    Ok(parent.join("workers"))
}

/// Ensure `workers_dir()` exists and contains the starter `claude.toml`
/// (dropped from [`STARTER_CLAUDE_MANIFEST`] on first run). Returns the
/// directory path either way.
///
/// Existing files are never overwritten — this is a fresh-install
/// helper, not a migration. Users who delete or edit their
/// `claude.toml` keep their version on subsequent starts.
pub fn ensure_workers_dir() -> Result<PathBuf> {
    let dir = workers_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating workers dir: {}", dir.display()))?;
    let starter = dir.join("claude.toml");
    if !starter.exists() {
        fs::write(&starter, STARTER_CLAUDE_MANIFEST)
            .with_context(|| format!("writing starter manifest: {}", starter.display()))?;
    }
    Ok(dir)
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

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct WakeConfig {
    /// Master switch: when false the daemon doesn't listen for a wake word
    /// at all. Hotkey-driven `jarvis listen` works regardless.
    pub enabled: bool,
    /// Which wake-word backend to use.
    /// Implemented today: `"none"`, `"whisper"`.
    /// Roadmap (return an error if selected today): `"sherpa"`,
    /// `"openwakeword"`, `"rustpotter"`.
    pub backend: String,
    /// Words or short phrases that trigger Jarvis. Matched case-insensitive
    /// after stripping accents, so `"mutombo"` matches `"¡Mutombo!"`.
    /// Empty for `backend = "none"`; required otherwise.
    pub phrases: Vec<String>,
    /// RMS threshold for the energy VAD: anything below counts as silence.
    /// Range: 0.0 (everything is speech) — 1.0 (nothing is). Sane: 0.015–0.05.
    /// This is the *trigger* threshold — once speech starts, the sustain
    /// threshold (threshold * `sustain_factor`) takes over so soft
    /// consonants in the middle of a word don't cut the utterance short.
    pub vad_rms_threshold: f32,
    /// Multiplier applied to `vad_rms_threshold` while a speech segment is
    /// already in progress. 0.5 means "once you've started talking, half
    /// the threshold is enough to keep me listening" — classic noise-gate
    /// hysteresis. Set to 1.0 to disable the hysteresis behaviour.
    pub sustain_factor: f32,
    /// Audio (in seconds) kept in a rolling pre-roll buffer and prepended
    /// to each captured utterance when speech is detected. Without this,
    /// the first ~100 ms of every word — where the VAD threshold is just
    /// being crossed — gets clipped, so "mutombo" becomes "Combo" and
    /// whisper hallucinates. 0.3 s catches typical consonant onsets.
    pub preroll_seconds: f32,
    /// Trailing silence (seconds) that ends a candidate utterance and
    /// triggers transcription.
    pub silence_seconds: f32,
    /// Cap on a single utterance — force-transcribe at this length even if
    /// the user is still talking.
    pub max_listen_seconds: f32,
    /// Backwards-compat / future use. CUDA / Vulkan / Metal kick in via the
    /// main STT config; this is reserved for backend-specific tuning later.
    pub cooldown_seconds: f32,
    /// Override the path of the Whisper ggml model used by the wake loop.
    /// Lets you pair a heavy main STT model (large-v3, 3 GB) with a tiny
    /// wake model (base or tiny, ~100 MB) so the always-on loop stays
    /// fast and light. Leave `None` to reuse `stt.model`.
    pub stt_model_override: Option<String>,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: "none".into(),
            phrases: vec!["jarvis".into()],
            // 0.02 RMS is forgiving: catches normal-volume speech at desk
            // distance without firing on typing/HVAC noise. The old 0.03
            // default required users to almost-shout into the mic.
            vad_rms_threshold: 0.02,
            sustain_factor: 0.5,
            preroll_seconds: 0.3,
            silence_seconds: 0.8,
            max_listen_seconds: 3.0,
            cooldown_seconds: 2.0,
            stt_model_override: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct RecordConfig {
    /// `auto` lets jarvis pick the first available recorder.
    pub backend: String,
    pub device: Option<String>,
    pub sample_rate: u32,
    pub channels: u32,
    pub max_seconds: f32,
    pub silence_seconds: f32,
    /// dBFS threshold below which audio is considered "silence" for the
    /// ffmpeg `silenceremove` filter. Larger (less negative) values are
    /// more permissive — they treat quiet-but-not-silent audio as silence
    /// and let the recorder terminate sooner on natural pauses. Default
    /// `-30.0` works on typical USB headsets and laptop mics; tighten to
    /// `-40.0` in a sound-treated room or loosen toward `-20.0` if your
    /// utterances get cut off mid-sentence because ambient noise prevents
    /// the silence detector from ever firing.
    pub silence_threshold_db: f32,
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
            // 4.0 s of sustained silence ends the turn. Multiple
            // bumps got us here: 1.5 (original) → 2.5 → 3.0 → 4.0.
            // 4.0 covers the "let me find the right word" pause
            // that some users naturally do mid-sentence, at the
            // cost of an extra second of perceived latency on
            // finished turns. Still feels conversational rather
            // than sluggish in live testing.
            silence_seconds: 4.0,
            // -40 dBFS: live testing showed that even at -35 the user's
            // natural inter-word RMS dips (consonant articulation, brief
            // breaths) fell below threshold and the onset detector
            // truncated utterances after silence_seconds. Idle on the
            // same rig measured -44 to -52 dBFS, so -40 still separates
            // speech (avg -25 to -33) from idle, with margin for the
            // articulation dips that motivated the change.
            silence_threshold_db: -40.0,
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    pub backend: String,
    pub binary: String,
    pub model: String,
    pub language: String,
    pub threads: u32,
    /// GPU acceleration. `None` = let whisper.cpp pick (the default — uses GPU
    /// if the binary was compiled with CUDA / Vulkan / Metal / ROCm).
    /// `Some(false)` adds `--no-gpu` to force CPU. `Some(true)` is a no-op
    /// today since whisper.cpp has no "force GPU" flag — set it for
    /// documentation / future-proofing.
    pub use_gpu: Option<bool>,
    /// Pick a specific GPU on multi-GPU systems. Maps to `--gpu-device N`.
    /// Only honoured by CUDA builds; Vulkan/Metal pick automatically.
    pub gpu_device: Option<u32>,
    /// Flash-attention. Speeds up decoding on Ampere/Hopper and Apple Silicon.
    /// Maps to `--flash-attn`. Safe to leave on if your hardware supports it;
    /// older GPUs will just ignore it.
    pub flash_attn: bool,
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
            use_gpu: None,
            gpu_device: None,
            flash_attn: false,
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
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

#[derive(Debug, Deserialize, Serialize, Clone)]
// NOTE: `deny_unknown_fields` is **not** compatible with `serde(flatten)`.
// AgentConfig accepts arbitrary keys past `name` and forwards them to the
// agent constructor — schema-strictness lives at the JarvisConfig / per-
// section level instead.
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

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    /// Master switch. `false` makes every agent turn stateless (no
    /// history fed in, nothing persisted). `true` keeps a rolling
    /// conversation in `~/.cache/jarvis/sessions/current.json`.
    pub enabled: bool,
    /// Idle seconds before the session is considered abandoned and a new
    /// one starts. 0 disables expiry. Default 30 min.
    pub ttl_seconds: u64,
    /// Hard cap on turns retained in memory. Older turns are dropped
    /// before each agent call so the prompt token budget stays bounded.
    /// 0 means stateless even with `enabled = true`.
    pub max_turns: usize,
    /// Voice phrases that, when transcribed as the entire user utterance
    /// (case-insensitive, accent-stripped), reset the session instead of
    /// being forwarded to the agent. Lets the user start over without
    /// going to the terminal.
    pub reset_phrases: Vec<String>,
    /// After each successful agent turn in daemon (wake-word) mode, how
    /// long to keep listening for a follow-up utterance without
    /// requiring the wake word again. 0 disables the follow-up window
    /// entirely. Short clarifications ("¿y en Tokio?") chain naturally
    /// without re-saying the wake phrase.
    pub followup_window_secs: f32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_seconds: 30 * 60,
            max_turns: 30,
            followup_window_secs: 10.0,
            reset_phrases: vec![
                "olvida".into(),
                "olvidalo".into(),
                "olvida todo".into(),
                "nueva conversacion".into(),
                "nueva conversación".into(),
                "new conversation".into(),
                "forget".into(),
                "forget everything".into(),
                "reset".into(),
            ],
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// How many terminal-status (completed/failed/cancelled/orphaned)
    /// tasks to keep on disk before the daemon's startup auto-prune
    /// drops the oldest. Active (Running) tasks are never affected by
    /// this cap.
    pub max_retained: usize,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            // Spec 0011: keep the last 50 terminal records by default;
            // generous enough for normal use, bounded enough to keep
            // the cache dir tidy.
            max_retained: 50,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct JarvisConfig {
    /// Schema version. See `CURRENT_CONFIG_VERSION` for the changelog.
    pub config_version: u32,
    pub log_level: String,
    pub speak_responses: bool,
    pub wake: WakeConfig,
    pub record: RecordConfig,
    pub stt: SttConfig,
    pub tts: TtsConfig,
    pub agent: AgentConfig,
    pub session: SessionConfig,
    pub tasks: TasksConfig,
}

impl Default for JarvisConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            log_level: "INFO".into(),
            speak_responses: true,
            wake: WakeConfig::default(),
            record: RecordConfig::default(),
            stt: SttConfig::default(),
            tts: TtsConfig::default(),
            agent: AgentConfig::default(),
            session: SessionConfig::default(),
            tasks: TasksConfig::default(),
        }
    }
}

/// Load and validate the config at `path`.
///
/// This is a two-pass operation:
/// 1. **Version probe** — parse the file as an untyped `toml::Table` and
///    read just `config_version`. If it's missing or older than
///    `CURRENT_CONFIG_VERSION`, bail with a migration message *before*
///    serde sees any unknown fields (which would give a worse error).
/// 2. **Typed parse** — once the version checks out, deserialise into the
///    real struct with `deny_unknown_fields` enabled so typos are caught.
pub fn load(path: &Path) -> Result<JarvisConfig> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("reading config: {}", path.display()))?;

    let probe: toml::Table = toml::from_str(&raw)
        .with_context(|| format!("config is not valid TOML: {}", path.display()))?;
    let found_version = probe
        .get("config_version")
        .and_then(|v| v.as_integer())
        .map(|i| i as u32)
        .unwrap_or(0);

    if found_version < CURRENT_CONFIG_VERSION {
        return Err(migration_error(path, found_version));
    }
    if found_version > CURRENT_CONFIG_VERSION {
        // The user has a newer jarvis binary's config file but is running an
        // older binary. Don't try to "downgrade" — refuse cleanly.
        return Err(anyhow!(
            "config_version {found} is newer than this binary supports (max {current}). \
             Upgrade jarvis or use an older config file. ({path})",
            found = found_version,
            current = CURRENT_CONFIG_VERSION,
            path = path.display()
        ));
    }

    let mut cfg: JarvisConfig =
        toml::from_str(&raw).with_context(|| format!("parsing config: {}", path.display()))?;

    // `JARVIS_AGENT=openai jarvis listen` overrides the configured agent
    // without editing the file. Handy for testing.
    if let Ok(agent) = std::env::var("JARVIS_AGENT") {
        cfg.agent.name = agent;
    }
    Ok(cfg)
}

fn migration_error(path: &Path, found: u32) -> anyhow::Error {
    anyhow!(
        "config_version {found} is older than this binary expects (v{current}).\n\n\
         The schema changed between releases — your config has fields the new \
         binary doesn't recognise (or vice versa). Run:\n\n\
         \x20   jarvis setup\n\n\
         to interactively regenerate the config (your old file will be backed up \
         to {path}.bak first), or, to discard your tweaks and start clean:\n\n\
         \x20   mv {path} {path}.bak\n\
         \x20   jarvis setup",
        found = found,
        current = CURRENT_CONFIG_VERSION,
        path = path.display()
    )
}
