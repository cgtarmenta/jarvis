//! `jarvis` command-line surface.
//!
//! Subcommands are intentionally small and orthogonal so a future GUI / TUI
//! front-end can call any of them programmatically (`jarvis listen` to run
//! one turn, `jarvis doctor` for the health pane, `jarvis test-agent` for an
//! agent ping, …).

use std::env;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::EnvFilter;

use crate::config::{self, JarvisConfig};
use crate::pipeline::run_once;

#[derive(Parser, Debug)]
#[command(
    name = "jarvis",
    about = "Always-on voice assistant orchestrator (wake/hotkey → STT → AI agent → TTS).",
    version,
    propagate_version = true
)]
struct Cli {
    /// Set log verbosity. Overrides `log_level` in config.
    #[arg(long, global = true, value_name = "LEVEL")]
    log: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a single turn: record once, transcribe, send to agent, speak reply.
    /// Bind this to a global hotkey in your WM for push-to-talk operation.
    Listen,

    /// Run the wake-word daemon (requires `[wake] enabled = true`).
    Daemon,

    /// Run the daemon in the foreground with debug logging — for development.
    Dev,

    /// Print the active config path and contents.
    Config,

    /// Open the config file in $EDITOR.
    EditConfig,

    /// Health check — confirm config, binaries, and the agent CLI are present.
    Doctor,

    /// One-shot TTS: speak the given text.
    TestTts {
        /// Text to speak. Defaults to a fixed phrase if omitted.
        #[arg(default_value = "Hello, this is Jarvis.")]
        text: String,
    },

    /// One-shot STT: record N seconds, transcribe, print the transcript.
    TestStt {
        #[arg(long, default_value_t = 4.0)]
        seconds: f32,
    },

    /// One-shot agent ping: send a text prompt to the configured agent and
    /// print the reply (no audio).
    TestAgent {
        /// The prompt. Joined with spaces if multi-word.
        #[arg(num_args = 1..)]
        prompt: Vec<String>,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    // Logging filter: CLI flag > $JARVIS_LOG > config value.
    let cfg_path = config::ensure_config()?;
    let cfg = config::load(&cfg_path)?;
    let log_level = cli
        .log
        .clone()
        .or_else(|| env::var("JARVIS_LOG").ok())
        .unwrap_or_else(|| cfg.log_level.clone());
    setup_logging(&log_level);
    tracing::debug!(config = %cfg_path.display(), "config loaded");

    match cli.cmd {
        Cmd::Listen => cmd_listen(&cfg),
        Cmd::Daemon => crate::daemon::run(cfg),
        Cmd::Dev => cmd_dev(&cfg_path),
        Cmd::Config => cmd_config(&cfg_path),
        Cmd::EditConfig => cmd_edit_config(&cfg_path),
        Cmd::Doctor => cmd_doctor(&cfg, &cfg_path),
        Cmd::TestTts { text } => cmd_test_tts(&cfg, &text),
        Cmd::TestStt { seconds } => cmd_test_stt(&cfg, seconds),
        Cmd::TestAgent { prompt } => cmd_test_agent(&cfg, &prompt.join(" ")),
    }
}

fn setup_logging(level: &str) {
    let lvl = match level.to_uppercase().as_str() {
        "ERROR" => Level::ERROR,
        "WARN" | "WARNING" => Level::WARN,
        "INFO" => Level::INFO,
        "DEBUG" => Level::DEBUG,
        "TRACE" => Level::TRACE,
        _ => Level::INFO,
    };
    // $JARVIS_LOG_FILTER lets users provide a full `tracing` filter string
    // (e.g. `jarvis=debug,ureq=warn`) for finer control than the flat level.
    let filter = env::var("JARVIS_LOG_FILTER")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| EnvFilter::new(lvl.to_string()));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn cmd_listen(cfg: &JarvisConfig) -> Result<()> {
    match run_once(cfg)? {
        Some(reply) if !reply.is_empty() => {
            println!("{reply}");
        }
        _ => {}
    }
    Ok(())
}

fn cmd_dev(cfg_path: &Path) -> Result<()> {
    // Setting an env var is unsafe in Rust 2024 because of POSIX setenv's
    // global mutability — but at this point we are still single-threaded
    // (main hasn't spawned any threads), so this is sound.
    unsafe {
        env::set_var("JARVIS_LOG", "DEBUG");
    }
    let cfg = config::load(cfg_path)?;
    println!("== jarvis dev mode (debug logging, hotkey-equivalent single turn) ==");
    cmd_listen(&cfg)
}

fn cmd_config(cfg_path: &Path) -> Result<()> {
    let contents = std::fs::read_to_string(cfg_path)?;
    println!("# Config file: {}\n{contents}", cfg_path.display());
    Ok(())
}

fn cmd_edit_config(cfg_path: &Path) -> Result<()> {
    let editor = env::var("EDITOR").unwrap_or_else(|_| "nano".into());
    std::process::Command::new(&editor)
        .arg(cfg_path)
        .status()
        .with_context(|| format!("running editor: {editor}"))?;
    Ok(())
}

fn cmd_doctor(cfg: &JarvisConfig, cfg_path: &Path) -> Result<()> {
    fn line(label: &str, ok: bool, detail: &str) {
        let tag = if ok { "OK" } else { "MISSING" };
        println!("  [{tag:<7}] {label:<24} {detail}");
    }

    println!("Jarvis doctor");
    line(
        "config file",
        cfg_path.exists(),
        &cfg_path.display().to_string(),
    );
    let piper = which::which(&cfg.tts.piper_binary).is_ok();
    line("piper binary", piper, &cfg.tts.piper_binary);
    let espeak = which::which("espeak-ng").is_ok() || which::which("espeak").is_ok();
    line("espeak-ng", espeak, "fallback TTS");
    let player = ["paplay", "pw-play", "aplay", "afplay"]
        .iter()
        .find(|p| which::which(p).is_ok())
        .copied()
        .unwrap_or("none");
    line("audio player", player != "none", player);

    let recorder = ["parecord", "pw-record", "arecord", "ffmpeg"]
        .iter()
        .find(|p| which::which(p).is_ok())
        .copied()
        .unwrap_or("none");
    line("recorder", recorder != "none", recorder);

    let whisper = which::which(&cfg.stt.binary).is_ok();
    line("STT binary", whisper, &cfg.stt.binary);
    line(
        "STT model",
        std::path::Path::new(&cfg.stt.model).is_file(),
        &cfg.stt.model,
    );

    match cfg.agent.name.as_str() {
        "claude" | "claude-code" => {
            let bin = cfg
                .agent
                .options
                .get("binary")
                .and_then(|v| v.as_str())
                .unwrap_or("claude");
            line(
                &format!("agent: {bin}"),
                which::which(bin).is_ok(),
                "Claude Code CLI",
            );
        }
        "openai" | "chatgpt" => {
            let ok = cfg.agent.options.contains_key("api_key")
                || env::var("OPENAI_API_KEY")
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
            line("OPENAI_API_KEY", ok, "env var or [agent].api_key");
        }
        "gemini" | "google" => {
            let ok = cfg.agent.options.contains_key("api_key")
                || env::var("GEMINI_API_KEY").is_ok()
                || env::var("GOOGLE_API_KEY").is_ok();
            line("GEMINI_API_KEY", ok, "env var or [agent].api_key");
        }
        "shell" => {
            let bin = cfg
                .agent
                .options
                .get("command")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("");
            line(
                &format!("agent: shell ({bin})"),
                !bin.is_empty() && which::which(bin).is_ok(),
                "shell agent command",
            );
        }
        other => line(&format!("agent: {other}"), false, "unknown agent"),
    }
    Ok(())
}

fn cmd_test_tts(cfg: &JarvisConfig, text: &str) -> Result<()> {
    let tts = crate::tts::build(cfg.tts.clone())?;
    tts.speak(text)
}

fn cmd_test_stt(cfg: &JarvisConfig, seconds: f32) -> Result<()> {
    println!("Recording {seconds:.1}s — speak now…");
    let mut rcfg = cfg.record.clone();
    rcfg.max_seconds = seconds;
    rcfg.silence_seconds = seconds + 1.0; // disable silence trim for the test
    let wav = crate::recorder::record_to_wav(&rcfg)?;
    let stt = crate::stt::build(cfg.stt.clone())?;
    let text = stt.transcribe(&wav)?;
    let _ = std::fs::remove_file(&wav);
    if text.is_empty() {
        return Err(anyhow!("STT returned empty transcription"));
    }
    println!("Heard: {text}");
    Ok(())
}

fn cmd_test_agent(cfg: &JarvisConfig, prompt: &str) -> Result<()> {
    let prompt = if prompt.is_empty() {
        "Say hello in one sentence."
    } else {
        prompt
    };
    let agent = crate::agents::build(cfg.agent.clone())?;
    println!("Prompt: {prompt}");
    let reply = agent.respond(prompt)?;
    println!("Reply:  {reply}");
    Ok(())
}
