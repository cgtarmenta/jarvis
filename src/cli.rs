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

    /// Interactive first-time setup: pick language, Whisper model, voice, agent.
    Setup,

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

    /// Speak the given text (or stdin) via the configured TTS backend.
    /// Use this as a voice-notification target from scripts, Claude Code
    /// `Stop` hooks, cron, anything that wants to say something out loud.
    ///
    /// Examples:
    ///   jarvis say "build finished"
    ///   echo "long summary" | jarvis say -
    ///   jarvis say --voice en_GB-alan-medium "all tests passed"
    Say {
        /// Text to speak. If the single arg is `-`, reads stdin until EOF.
        /// Multiple words are joined with spaces.
        #[arg(num_args = 0..)]
        text: Vec<String>,
        /// Override `[tts].voice` for this call only. Useful for trying a
        /// voice without editing config.
        #[arg(long)]
        voice: Option<String>,
        /// Return immediately and play the TTS in a detached background
        /// process. Use this inside Claude Code Stop hooks, cron jobs,
        /// or anywhere the parent is blocked waiting for the command to
        /// exit — without `--detach`, hooks hang for the duration of
        /// the spoken phrase.
        #[arg(long)]
        detach: bool,
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

    /// Inspect or manage the conversation session that the daemon and
    /// `listen` use to maintain context across turns.
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },

    /// Spec-driven development: manage specs in `specs/` (inbox / active /
    /// shipped / rejected). The voice-driven shortcuts ("open a spec
    /// for X", "promote 14") go through `jarvis listen` / `jarvis daemon`
    /// — these subcommands give you the same operations from a terminal.
    Spec {
        #[command(subcommand)]
        cmd: SpecCmd,
    },

    /// Manage external-agent sessions. Dispatches based on
    /// `[agent].name` in config:
    ///
    /// * `claude` / `claude-code` — Claude Code session attach via
    ///   `claude --print --resume <uuid>`. All four verbs supported.
    /// * `warp` / `oz` — roadmap (no-op for now).
    /// * `openai` / `gemini` / `shell` — agents are stateless from
    ///   Jarvis's POV; use `jarvis session` to inspect Jarvis's
    ///   conversation history instead.
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },

    /// Diagnostic: run the configured wake backend for N seconds with
    /// verbose logging (RMS readings, transcripts, match status). Use this
    /// to tune `[wake].vad_rms_threshold` and `[wake].phrases` without
    /// running the full pipeline. Exits on first wake event or timeout.
    TestWake {
        /// How long to keep listening (seconds). Defaults to 30.
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Override `[wake].vad_rms_threshold` for this run only. Useful
        /// when iterating: each run prints the peak RMS observed, you
        /// adjust here, retry — no config edits between attempts.
        #[arg(long)]
        threshold: Option<f32>,
        /// Override `[wake].phrases` for this run only. Comma-separated.
        #[arg(long)]
        phrases: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCmd {
    /// Print the active session (id, age, turn count, last few turns).
    Show,
    /// Wipe the active session — next turn starts a new conversation.
    Reset,
    /// Print the absolute path to the session JSON file.
    Path,
}

#[derive(Subcommand, Debug)]
enum AgentCmd {
    /// List every external agent session available for resumption.
    /// For the claude agent: every JSONL under `~/.claude/projects/`,
    /// newest first. With `--cwd`, restrict to one project.
    Sessions {
        /// Filter to the project at this working dir (encoded the same
        /// way Claude Code itself encodes it on disk).
        #[arg(long)]
        cwd: Option<String>,
        /// How many sessions to show. Default 20 to keep the list usable.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Pin Jarvis to a specific external-agent session UUID.
    /// Subsequent voice turns resume that session via the agent's
    /// native mechanism (e.g. `claude --print --resume <uuid>`).
    ///
    /// Without arguments, prints the currently pinned attachment.
    /// With `--latest`, instead saves auto-resume mode targeting the
    /// session newest at each turn (so the pin moves forward as you
    /// keep working in the same project).
    Attach {
        /// UUID (or unambiguous UUID prefix) of the session to pin.
        uuid: Option<String>,
        /// Save "always pick the newest session in `cwd`" instead of
        /// pinning a specific UUID.
        #[arg(long)]
        latest: bool,
        /// Working directory whose project namespace to attach to.
        /// Required with `--latest` if no `[agent].cwd` is set; for
        /// pinned UUIDs the cwd is recovered from the session's path.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Forget the pinned attachment. The configured agent reverts to
    /// whatever `[agent]` config says (`auto_resume = true` keeps
    /// auto-mode; otherwise the agent goes stateless).
    Detach,
    /// Print which session (if any) the daemon would resume right now.
    Status,
}

#[derive(Subcommand, Debug)]
enum SpecCmd {
    /// Create a new spec in `specs/inbox/` with the given title.
    New {
        /// Free-text title. Joined with spaces if multiple words.
        #[arg(num_args = 1..)]
        title: Vec<String>,
    },
    /// Print every spec grouped by status.
    List {
        /// Filter to a single status: inbox | active | shipped | rejected.
        #[arg(long)]
        status: Option<String>,
    },
    /// Print one spec by numeric id or slug fragment.
    Show { query: String },
    /// Move an inbox spec to active/, assigning the next sequential id.
    Promote { query: String },
    /// Move an active spec to shipped/. Requires every `## What` bullet
    /// to be checked.
    Ship { query: String },
    /// Move a spec to rejected/. Reason is recorded in the body.
    Reject {
        query: String,
        /// Why the spec was rejected. Joined with spaces.
        #[arg(num_args = 1..)]
        reason: Vec<String>,
    },
    /// Print the `specs/` directory path.
    Path,
    /// Open the spec file in $EDITOR.
    Edit { query: String },
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
        Cmd::Setup => crate::setup::run(),
        Cmd::Config => cmd_config(&cfg_path),
        Cmd::EditConfig => cmd_edit_config(&cfg_path),
        Cmd::Doctor => cmd_doctor(&cfg, &cfg_path),
        Cmd::TestTts { text } => cmd_test_tts(&cfg, &text),
        Cmd::Say {
            text,
            voice,
            detach,
        } => cmd_say(&cfg, text, voice.as_deref(), detach),
        Cmd::TestStt { seconds } => cmd_test_stt(&cfg, seconds),
        Cmd::TestAgent { prompt } => cmd_test_agent(&cfg, &prompt.join(" ")),
        Cmd::Session { cmd } => cmd_session(cmd),
        Cmd::Spec { cmd } => cmd_spec(cmd),
        Cmd::Agent { cmd } => cmd_agent(&cfg, cmd),
        Cmd::TestWake {
            seconds,
            threshold,
            phrases,
        } => cmd_test_wake(&cfg, seconds, threshold, phrases.as_deref()),
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
    let piper_present = which::which(&cfg.tts.piper_binary).is_ok();
    let piper_issue = if piper_present {
        crate::tts::piper_binary_issue(&cfg.tts.piper_binary)
    } else {
        None
    };
    let piper_ok = piper_present && piper_issue.is_none();
    let piper_detail = match (piper_present, piper_issue.as_deref()) {
        (false, _) => cfg.tts.piper_binary.clone(),
        (true, Some(issue)) => format!("{} — {issue}", cfg.tts.piper_binary),
        (true, None) => cfg.tts.piper_binary.clone(),
    };
    line("piper binary", piper_ok, &piper_detail);
    if piper_present && piper_issue.is_some() {
        println!(
            "    → install piper-tts (AUR): yay -S piper-tts \
             (may require removing the gaming-mice `piper` first)"
        );
    }
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
    let stt_model_ok = std::path::Path::new(&cfg.stt.model).is_file();
    line("STT model", stt_model_ok, &cfg.stt.model);
    if !stt_model_ok {
        println!("    → run `jarvis setup` to pick and download a model");
    }

    // Probe GPU support. whisper.cpp only prints the specific backend name
    // ("ggml_vulkan: ...", "ggml_cuda_init: ...") in stderr *when actually
    // decoding*, not in `--help`. What we *can* check cheaply is whether
    // the `--no-gpu` / `--device` flags exist — their presence means at
    // least one GPU backend is compiled in. To name it we'd have to run
    // an actual transcription; that's what `jarvis test-stt` is for.
    if which::which(&cfg.stt.binary).is_ok() {
        let probe = std::process::Command::new(&cfg.stt.binary)
            .arg("--help")
            .output();
        match probe {
            Ok(out) => {
                let help =
                    String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
                let has_gpu_flags = help.contains("--no-gpu") || help.contains("--device");
                let detail = if has_gpu_flags {
                    "GPU backend compiled in (run `jarvis test-stt` to see which)".to_string()
                } else {
                    "CPU only (--no-gpu flag absent from --help)".to_string()
                };
                line("STT GPU", has_gpu_flags, &detail);
            }
            Err(_) => line("STT GPU", false, "could not probe whisper-cli --help"),
        }
    }

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
            // Surface the active claude session attachment so the user
            // can confirm at a glance which conversation voice will hit.
            use crate::agents::claude_attach;
            let cfg_cwd = cfg.agent.options.get("cwd").and_then(|v| v.as_str());
            let cfg_auto = cfg
                .agent
                .options
                .get("auto_resume")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let state = claude_attach::load_state().ok().flatten();
            let attachment = claude_attach::resolve(state.as_ref(), cfg_cwd, cfg_auto);
            let detail = match &attachment {
                claude_attach::Attachment::None => "stateless".to_string(),
                claude_attach::Attachment::Pinned(uuid) => format!("pinned {uuid}"),
                claude_attach::Attachment::Latest { cwd } => {
                    match claude_attach::latest_session_for(cwd) {
                        Some(s) => format!("auto-resume → {} ({})", s.uuid, cwd.display()),
                        None => format!("auto-resume in {} (no session yet)", cwd.display()),
                    }
                }
            };
            line(
                "agent attach",
                !matches!(attachment, claude_attach::Attachment::None),
                &detail,
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
        "warp" | "oz" => {
            let bin = cfg
                .agent
                .options
                .get("binary")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    ["oz", "oz-preview", "warp-cli"]
                        .into_iter()
                        .find(|b| which::which(b).is_ok())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "oz".into());
            line(
                &format!("agent: {bin}"),
                which::which(&bin).is_ok(),
                "Warp oz CLI",
            );
            let auth_ok = cfg.agent.options.contains_key("api_key")
                || env::var("WARP_API_KEY")
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
            line("WARP_API_KEY", auth_ok, "env var or [agent].api_key");
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

/// `jarvis say [TEXT...] [--voice X]` — speak the argument (or stdin) via
/// the configured TTS. Designed to be a drop-in notification target for
/// any script that wants a voice cue:
///
/// ```ignore
/// cargo build && jarvis say "build ok"
/// claude --print 'summarise' | jarvis say -
/// ```
fn cmd_say(cfg: &JarvisConfig, text: Vec<String>, voice: Option<&str>, detach: bool) -> Result<()> {
    // `--detach` re-execs ourselves without the flag, redirects all
    // stdio to /dev/null, and exits the parent right after spawning.
    // The child then runs the normal synchronous TTS path while
    // whatever parent invoked us (a Claude Code Stop hook, cron, etc.)
    // gets exit 0 instantly. Without this, the hook hangs until the
    // spoken phrase finishes — which Claude Code shows as "thinking".
    if detach {
        let exe = std::env::current_exe().context("locating own binary for --detach")?;
        // Reconstruct argv without --detach so the child doesn't loop.
        let mut child_args: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| a != "--detach")
            .collect();
        // If the user wrote `jarvis say --detach -` with stdin, we
        // need to materialise stdin *now* (parent's stdin) before we
        // redirect the child's stdin to /dev/null, otherwise the
        // text is lost. Convert `-` into the literal text we read.
        if child_args.iter().any(|a| a == "-") {
            let buffered = read_stdin()?;
            for arg in child_args.iter_mut() {
                if arg == "-" {
                    *arg = buffered.clone();
                }
            }
        }
        std::process::Command::new(exe)
            .args(&child_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawning detached jarvis say")?;
        return Ok(());
    }

    // Resolve the text. Either:
    //   * no args, or the single arg `-`  → read stdin until EOF (pipes)
    //   * positional args                 → join with spaces
    //
    // Reading stdin on no-args matches the typical `cat`/`tee` pattern
    // where omitting the input file means "read from stdin".
    let resolved = if text.is_empty() || text == ["-"] {
        read_stdin()?
    } else {
        text.join(" ")
    };

    // Empty input is a no-op — exit 0 silently. Otherwise `cargo build &&
    // jarvis say ""` would error on the failure-but-no-output edge case.
    if resolved.trim().is_empty() {
        return Ok(());
    }

    // `--voice` override clones the TtsConfig instead of mutating the
    // loaded one in place so we never accidentally leak state across
    // commands.
    let mut tts_cfg = cfg.tts.clone();
    if let Some(v) = voice {
        tts_cfg.voice = v.to_string();
    }

    let tts = crate::tts::build(tts_cfg)?;
    tts.speak(&resolved)
}

fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
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

fn cmd_test_wake(
    cfg: &JarvisConfig,
    seconds: u64,
    threshold_override: Option<f32>,
    phrases_override: Option<&str>,
) -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Apply CLI overrides on a clone — the on-disk config is left alone so
    // users can iterate freely: `test-wake --threshold 0.015`,
    // `test-wake --threshold 0.01`, etc., without ever touching config.toml.
    let mut wake_cfg = cfg.wake.clone();
    if let Some(t) = threshold_override {
        wake_cfg.vad_rms_threshold = t;
    }
    if let Some(p) = phrases_override {
        wake_cfg.phrases = p
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    let backend = crate::wake::build(wake_cfg.clone(), cfg.stt.clone())?;
    println!(
        "▶ Listening for {seconds}s with backend={:?}, phrases={:?}, threshold={}",
        backend.name(),
        wake_cfg.phrases,
        wake_cfg.vad_rms_threshold
    );
    println!(
        "  STT model={}, language={}",
        cfg.stt.model, cfg.stt.language
    );
    println!(
        "  Say one of the phrases or wait for timeout. The log below will show \
         RMS levels, detected speech, and whisper transcripts."
    );
    println!();

    let stop = Arc::new(AtomicBool::new(false));
    // A timer thread flips the stop flag after `seconds`. The wake backend
    // polls `should_stop` between audio chunks so termination is responsive
    // without needing a signal handler at the test-command level.
    let stop_for_timer = Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
        stop_for_timer.store(true, Ordering::Relaxed);
    });

    let triggered = Arc::new(AtomicBool::new(false));
    let triggered_for_cb = Arc::clone(&triggered);
    let stop_for_cb = Arc::clone(&stop);
    let mut on_wake = move || {
        triggered_for_cb.store(true, Ordering::Relaxed);
        // First match wins — flip stop so the backend returns.
        stop_for_cb.store(true, Ordering::Relaxed);
    };

    let stop_for_check = Arc::clone(&stop);
    backend.run(&mut on_wake, &|| stop_for_check.load(Ordering::Relaxed))?;

    println!();
    if triggered.load(Ordering::Relaxed) {
        println!("✓ Wake phrase matched.");
    } else {
        println!(
            "✗ No wake phrase matched in {seconds}s. Check the log above:\n\
             \x20  - Is `peak_rms` consistently below your threshold? Lower it.\n\
             \x20  - Are transcripts unrelated to what you said? Speak closer to the mic.\n\
             \x20  - Did you see no \"speech started\" lines at all? Mic isn't being captured."
        );
    }
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
    // Pass empty history for one-shot test — `test-agent` is meant for
    // ping-style checks, not full conversation continuity. Use
    // `jarvis listen` / `jarvis daemon` for session-backed turns.
    let reply = agent.respond(prompt, &[])?;
    println!("Reply:  {reply}");
    Ok(())
}

fn cmd_session(cmd: SessionCmd) -> Result<()> {
    use crate::session;
    match cmd {
        SessionCmd::Path => {
            let p = session::session_path()?;
            println!("{}", p.display());
        }
        SessionCmd::Reset => {
            session::reset()?;
            println!("✓ session reset");
        }
        SessionCmd::Show => {
            let p = session::session_path()?;
            if !p.exists() {
                println!("No active session ({}).", p.display());
                return Ok(());
            }
            // Load with TTL=0 (disable expiry for inspection — we want
            // to see the file even if it's older than the runtime cap).
            let sess = session::load_or_new(0)?;
            println!("Session: {}", sess.id);
            println!("  path:          {}", p.display());
            println!("  started_at:    {}", sess.started_at);
            println!("  last_activity: {}", sess.last_activity);
            println!("  turns:         {}", sess.turns.len());
            if !sess.turns.is_empty() {
                let tail = sess.turns.iter().rev().take(6).collect::<Vec<_>>();
                println!();
                println!("Most recent (last 6, newest first):");
                for t in tail {
                    let preview: String = t.content.chars().take(140).collect();
                    let suffix = if t.content.chars().count() > 140 {
                        " …"
                    } else {
                        ""
                    };
                    println!("  {:<10} {}{}", format!("[{:?}]", t.role), preview, suffix);
                }
            }
        }
    }
    Ok(())
}

fn cmd_spec(cmd: SpecCmd) -> Result<()> {
    use crate::specs::{Status, store};

    let specs_dir = store::find_specs_dir_from_cwd()
        .context("locating specs/ — run from a jarvis-style repo, or create specs/ first")?;

    match cmd {
        SpecCmd::Path => {
            println!("{}", specs_dir.display());
        }
        SpecCmd::New { title } => {
            let title = title.join(" ");
            let s = store::create_inbox(&specs_dir, &title)?;
            println!("✓ created {}", s.path.display());
        }
        SpecCmd::List { status } => {
            let filter = match status.as_deref() {
                Some(s) => Some(Status::parse(s).ok_or_else(|| anyhow!("unknown status {s:?}"))?),
                None => None,
            };
            let mut all = store::list_all(&specs_dir)?;
            // Stable display order: inbox, active, shipped, rejected; then
            // by id ascending (with un-IDed inbox specs sorted by filename).
            all.sort_by(|a, b| {
                let sa = a.frontmatter.status.unwrap_or(Status::Inbox);
                let sb = b.frontmatter.status.unwrap_or(Status::Inbox);
                let order = |s: Status| match s {
                    Status::Inbox => 0,
                    Status::Active => 1,
                    Status::Shipped => 2,
                    Status::Rejected => 3,
                    Status::Private => 4,
                };
                order(sa).cmp(&order(sb)).then(
                    a.frontmatter
                        .id
                        .unwrap_or(0)
                        .cmp(&b.frontmatter.id.unwrap_or(0))
                        .then_with(|| a.path.cmp(&b.path)),
                )
            });
            if all.is_empty() {
                println!("No specs found in {}.", specs_dir.display());
                return Ok(());
            }
            print_spec_table(&all, filter);
        }
        SpecCmd::Show { query } => {
            let s = store::find(&specs_dir, &query)?
                .ok_or_else(|| anyhow!("no spec matches {query:?}"))?;
            print_spec_detail(&s);
        }
        SpecCmd::Promote { query } => {
            let s = store::find(&specs_dir, &query)?
                .ok_or_else(|| anyhow!("no spec matches {query:?}"))?;
            let promoted = store::promote(&specs_dir, &s)?;
            println!(
                "✓ promoted {:04} {}",
                promoted.frontmatter.id.unwrap_or(0),
                promoted.path.display()
            );
        }
        SpecCmd::Ship { query } => {
            let s = store::find(&specs_dir, &query)?
                .ok_or_else(|| anyhow!("no spec matches {query:?}"))?;
            let shipped = store::ship(&specs_dir, &s)?;
            println!("✓ shipped {}", shipped.path.display());
        }
        SpecCmd::Reject { query, reason } => {
            let s = store::find(&specs_dir, &query)?
                .ok_or_else(|| anyhow!("no spec matches {query:?}"))?;
            let reason = reason.join(" ");
            if reason.trim().is_empty() {
                return Err(anyhow!(
                    "rejecting a spec requires a reason — `jarvis spec reject <id> <reason>`"
                ));
            }
            let rejected = store::reject(&specs_dir, &s, &reason)?;
            println!("✓ rejected {}", rejected.path.display());
        }
        SpecCmd::Edit { query } => {
            let s = store::find(&specs_dir, &query)?
                .ok_or_else(|| anyhow!("no spec matches {query:?}"))?;
            let editor = env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            std::process::Command::new(&editor)
                .arg(&s.path)
                .status()
                .with_context(|| format!("running editor: {editor}"))?;
        }
    }
    Ok(())
}

fn print_spec_table(all: &[crate::specs::spec::Spec], filter: Option<crate::specs::Status>) {
    use crate::specs::Status;
    let mut current: Option<Status> = None;
    for s in all {
        let st = s.frontmatter.status.unwrap_or(Status::Inbox);
        if let Some(want) = filter
            && want != st
        {
            continue;
        }
        if current != Some(st) {
            current = Some(st);
            println!();
            println!("[{}]", st.dir());
        }
        let id = s
            .frontmatter
            .id
            .map(|n| format!("{n:04}"))
            .unwrap_or_else(|| "    ".to_string());
        let name = s.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        println!("  {id}  {name}  — {}", s.frontmatter.title);
    }
}

fn print_spec_detail(s: &crate::specs::spec::Spec) {
    println!("{}", s.path.display());
    println!();
    println!("{}", s.serialize());
}

/// Per-agent dispatcher for `jarvis agent <verb>`. Inspects
/// `cfg.agent.name` and routes to the right backend's handler, or
/// emits a polite explanation when the configured agent has no
/// external session concept.
fn cmd_agent(cfg: &JarvisConfig, cmd: AgentCmd) -> Result<()> {
    match cfg.agent.name.as_str() {
        "claude" | "claude-code" => cmd_agent_claude(cfg, cmd),
        "warp" | "oz" => {
            agent_unsupported(
                &cfg.agent.name,
                "warp session attach is on the roadmap but not yet implemented \
                 (see specs/inbox for status).",
            );
            Ok(())
        }
        "openai" | "chatgpt" | "gemini" | "google" => {
            agent_unsupported(
                &cfg.agent.name,
                "this agent is stateless from Jarvis's POV — there's no \
                 external session to attach to. Use `jarvis session` to \
                 inspect / reset the Jarvis-side conversation history.",
            );
            Ok(())
        }
        "shell" => {
            agent_unsupported(
                "shell",
                "the shell agent's session model depends on the command \
                 you wired up. Manage Jarvis-side history with \
                 `jarvis session` instead.",
            );
            Ok(())
        }
        other => {
            agent_unsupported(
                other,
                "unknown agent name — check `[agent].name` in your config.",
            );
            Ok(())
        }
    }
}

/// Friendly informational printout for verbs that don't apply to the
/// configured agent. Exit code stays 0 — this is documentation, not
/// failure.
fn agent_unsupported(agent_name: &str, message: &str) {
    println!("agent {agent_name:?}: {message}");
}

/// Claude-specific dispatcher. Kept distinct from `cmd_agent` so the
/// per-agent logic stays in its own block and is easy to extend when
/// other agents grow session support.
fn cmd_agent_claude(cfg: &JarvisConfig, cmd: AgentCmd) -> Result<()> {
    use crate::agents::claude_attach;
    use std::path::Path;

    // Read the configured claude `cwd` once so attach/status can fall
    // back to it. None means "use the current directory".
    let config_cwd: Option<String> = cfg
        .agent
        .options
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    match cmd {
        AgentCmd::Sessions { cwd, limit } => {
            let filter = cwd.as_deref().map(Path::new);
            let sessions = claude_attach::list_sessions(filter)?;
            if sessions.is_empty() {
                println!(
                    "No Claude sessions found under {}.",
                    claude_attach::claude_home().join("projects").display()
                );
                return Ok(());
            }
            println!("uuid                                  age(min)   size  first user message");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for s in sessions.into_iter().take(limit) {
                let age_min = now.saturating_sub(s.mtime) / 60;
                let size_kb = s.size_bytes / 1024;
                let preview: String = s.first_user_message.chars().take(60).collect();
                println!(
                    "{:<36}  {:>10}  {:>5}K  {}",
                    s.uuid, age_min, size_kb, preview
                );
            }
        }

        AgentCmd::Attach { uuid, latest, cwd } => {
            // `--latest` and a specific UUID are mutually exclusive.
            if latest && uuid.is_some() {
                return Err(anyhow!(
                    "`--latest` and an explicit UUID can't both be set; pick one"
                ));
            }
            // No-arg form prints the current attachment.
            if !latest && uuid.is_none() {
                return cmd_agent_claude(cfg, AgentCmd::Status);
            }

            let state = if latest {
                let cwd = cwd.or(config_cwd.clone()).unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| ".".into())
                });
                claude_attach::AttachState {
                    session_id: None,
                    auto_resume: true,
                    cwd: Some(cwd),
                }
            } else {
                let raw = uuid.unwrap();
                // Accept unambiguous prefix: scan all sessions, match.
                let resolved = if raw.len() == 36 && raw.contains('-') {
                    raw
                } else {
                    let candidates: Vec<_> = claude_attach::list_sessions(None)?
                        .into_iter()
                        .filter(|s| s.uuid.starts_with(&raw))
                        .collect();
                    match candidates.len() {
                        0 => return Err(anyhow!("no session matches prefix {raw:?}")),
                        1 => candidates.into_iter().next().unwrap().uuid,
                        n => {
                            return Err(anyhow!(
                                "prefix {raw:?} is ambiguous ({n} matches); pass more chars"
                            ));
                        }
                    }
                };
                claude_attach::AttachState {
                    session_id: Some(resolved),
                    auto_resume: false,
                    cwd,
                }
            };
            claude_attach::save_state(&state)?;
            println!("✓ attached");
            cmd_agent_claude(cfg, AgentCmd::Status)?;
        }

        AgentCmd::Detach => {
            claude_attach::clear_state()?;
            println!("✓ detached — agent reverts to [agent] config defaults");
        }

        AgentCmd::Status => {
            let auto_in_cfg = cfg
                .agent
                .options
                .get("auto_resume")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let state = claude_attach::load_state()?;
            let attachment =
                claude_attach::resolve(state.as_ref(), config_cwd.as_deref(), auto_in_cfg);
            match attachment {
                claude_attach::Attachment::None => {
                    println!("Stateless — each voice turn starts a fresh claude --print.");
                }
                claude_attach::Attachment::Pinned(uuid) => {
                    println!("Pinned to session {uuid}");
                }
                claude_attach::Attachment::Latest { cwd } => {
                    println!("Auto-resume in {}", cwd.display());
                    match claude_attach::latest_session_for(&cwd) {
                        Some(s) => println!("  → will resume {} (mtime={})", s.uuid, s.mtime),
                        None => println!("  → no sessions found in that project yet"),
                    }
                }
            }
        }
    }
    Ok(())
}
