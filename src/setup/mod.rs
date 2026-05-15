//! `jarvis setup` — interactive first-time configuration wizard.
//!
//! The wizard is **non-destructive**: it loads the current config (creating
//! one from the bundled example if needed), walks the user through each
//! decision with sensible defaults derived from `$LANG`, downloads any
//! missing model / voice files, and then writes the config back to disk.
//!
//! It also serialises the result *back to TOML* by re-rendering the file
//! rather than monkey-patching the example. That keeps the schema canonical
//! and avoids comment-preservation bugs (the example stays in `/usr/share`
//! for reference; the user's file is the working copy).

mod deps;
mod dispatcher;
mod locale;
mod voices;
mod whisper;

pub use self::dispatcher::{looks_like_http_url, normalize_user_input};

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password, Select};
use tracing::{info, warn};

use crate::config::{self, JarvisConfig};
use crate::wake as wake_backends;

use self::locale::{Defaults, Locale};
use self::voices::Voice;
use self::whisper::ModelInfo;

/// Entry point for the `setup` CLI command.
pub fn run() -> Result<()> {
    let theme = ColorfulTheme::default();
    let cfg_path = config::ensure_config()?;

    // Try to load existing config. If it's an old/incompatible schema we
    // can't deserialise — back it up and start from defaults so the wizard
    // can write a fresh file. This is the entire point of `jarvis setup`
    // existing as a separate path from the daemon: it's allowed to recover
    // from broken configs by regenerating them.
    let mut cfg = match config::load(&cfg_path) {
        Ok(c) => c,
        Err(err) => {
            println!("⚠ Existing config could not be loaded:");
            println!("  {err:#}");
            println!();
            // Heads-up about `[dispatcher.fallback]` before the .bak rotation
            // — saves the user having to dig through `.toml.bak` to recover
            // their stage-2 config when the failure is unrelated to it (e.g.
            // a schema-version bump invalidates the whole file).
            warn_if_had_dispatcher_fallback(&cfg_path);
            backup_existing(&cfg_path)?;
            config::JarvisConfig::default()
        }
    };

    println!();
    println!("Jarvis setup — let's get this configured.");
    println!("Config file: {}", cfg_path.display());
    println!();

    // 1. Locale ------------------------------------------------------------
    let detected = locale::detect();
    println!("Detected system locale: {}", detected.pretty());
    let locale = if Confirm::with_theme(&theme)
        .with_prompt("Use this language for STT and the default voice?")
        .default(true)
        .interact()?
    {
        detected
    } else {
        ask_locale(&theme)?
    };
    let defaults = locale::defaults_for(&locale);

    // 2. Whisper model -----------------------------------------------------
    println!();
    println!("== Speech-to-text (whisper.cpp) ==");
    // Check that the binary is on PATH before downloading a model — there's
    // no point pulling 142 MB of ggml weights if the user can't run them.
    // If the user skipped the install we still proceed: the model download
    // is harmless, and they may install whisper.cpp later.
    let _stt_status = deps::ensure_installed(&theme, &deps::WHISPER_CLI)?;
    let model = choose_whisper_model(&theme, &defaults)?;
    let model_path = whisper::ensure_downloaded(&model)
        .with_context(|| format!("downloading whisper model {}", model.id))?;
    info!(model = %model_path.display(), "whisper model ready");
    cfg.stt.backend = "whisper-cli".into();
    cfg.stt.binary = "whisper-cli".into();
    cfg.stt.model = model_path.to_string_lossy().into_owned();
    cfg.stt.language = if model.english_only {
        "en".into()
    } else {
        defaults.whisper_language.clone()
    };

    // Optional second model just for the wake loop. We only offer this
    // when the main model is medium/large — for tiny/base/small the
    // savings don't justify the extra disk. Setting an override here
    // populates `[wake].stt_model_override` so the always-on listener
    // uses the smaller model while `jarvis listen` keeps the heavy one.
    if matches!(model.id, "medium" | "large-v3") {
        println!();
        let want_separate = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "You picked `{}` ({} MB) — heavy for the always-on wake loop. \
                 Use a smaller model just for wake-word detection?",
                model.id, model.size_mb
            ))
            .default(true)
            .interact()?;
        if want_separate {
            let wake_model = choose_wake_model(&theme, &defaults)?;
            let wake_path = whisper::ensure_downloaded(&wake_model)
                .with_context(|| format!("downloading wake model {}", wake_model.id))?;
            info!(
                wake_model = %wake_path.display(),
                main_model = %model_path.display(),
                "wake-loop model decoupled from main STT"
            );
            cfg.wake.stt_model_override = Some(wake_path.to_string_lossy().into_owned());
        } else {
            cfg.wake.stt_model_override = None;
        }
    }

    // 3. TTS ---------------------------------------------------------------
    println!();
    println!("== Text-to-speech (piper) ==");
    let tts_status = deps::ensure_installed(&theme, &deps::PIPER_TTS)?;
    let piper_available = match tts_status {
        deps::DepStatus::Installed {
            binary_path: Some(p),
        } => {
            // pipx (or another user-space install) put piper at a
            // non-standard path. Write the absolute path back to config so
            // jarvis doesn't depend on PATH ordering vs the gaming-mice
            // piper that may still live in /usr/bin.
            cfg.tts.piper_binary = p.to_string_lossy().into_owned();
            println!(
                "  → recording piper path in config: {}",
                cfg.tts.piper_binary
            );
            true
        }
        deps::DepStatus::Installed { binary_path: None } => {
            cfg.tts.piper_binary = "piper".into();
            true
        }
        deps::DepStatus::Skipped => {
            // User skipped piper — offer espeak-ng as a fallback. We still
            // ask for a piper voice afterwards so it's saved in config and
            // ready when they later install piper.
            let use_espeak = Confirm::with_theme(&theme)
                .with_prompt(
                    "Skipped piper. Use espeak-ng for now (lower quality but always works)?",
                )
                .default(true)
                .interact()?;
            if use_espeak {
                cfg.tts.backend = "espeak".into();
                cfg.tts.espeak_voice = defaults.whisper_language.clone();
                println!("  → tts.backend = \"espeak\"");
            } else {
                cfg.tts.backend = "piper".into();
                println!(
                    "  ⚠ Leaving tts.backend = \"piper\" but piper isn't \
                     installed — TTS will error until you install it."
                );
            }
            false
        }
    };
    if piper_available {
        cfg.tts.backend = "piper".into();
    }
    let voice = choose_piper_voice(&theme, &defaults)?;
    cfg.tts.voice = voice;

    // 4. Wake word ---------------------------------------------------------
    println!();
    println!("== Wake word (always-listening trigger) ==");
    configure_wake(&theme, &mut cfg)?;

    // 5. Agent -------------------------------------------------------------
    println!();
    println!("== AI agent ==");
    configure_agent(&theme, &mut cfg)?;
    // After picking an agent, check that its CLI is present (where applicable).
    // If a user-space install dropped the binary in a non-standard place,
    // forward the absolute path to [agent].binary so the agent factory
    // doesn't have to re-discover it.
    let agent_status = match cfg.agent.name.as_str() {
        "claude" | "claude-code" => Some(deps::ensure_installed(&theme, &deps::CLAUDE_CODE)?),
        "warp" | "oz" => Some(deps::ensure_installed(&theme, &deps::WARP_OZ)?),
        _ => None,
    };
    if let Some(deps::DepStatus::Installed {
        binary_path: Some(p),
    }) = agent_status
    {
        cfg.agent.options.insert(
            "binary".into(),
            toml::Value::String(p.to_string_lossy().into_owned()),
        );
    }

    // 6. Dispatcher fallback (optional stage 2) ----------------------------
    println!();
    println!("== Dispatcher fallback (stage 2 LLM classifier) ==");
    dispatcher::configure_dispatcher_fallback(&theme, &mut cfg)?;

    // 7. Save --------------------------------------------------------------
    println!();
    save_config(&cfg, &cfg_path)?;
    println!("Saved config to {}", cfg_path.display());

    println!();
    println!("Next steps:");
    println!("  jarvis doctor              # confirm everything is wired");
    println!("  jarvis test-agent \"hi\"     # ping the agent");
    println!("  jarvis listen              # one full voice turn (bind to a hotkey)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Locale step
// ---------------------------------------------------------------------------

fn ask_locale(theme: &ColorfulTheme) -> Result<Locale> {
    let choices = [
        ("English", "en", "GB"),
        ("Spanish", "es", "ES"),
        ("French", "fr", "FR"),
        ("German", "de", "DE"),
        ("Italian", "it", "IT"),
        ("Portuguese", "pt", "PT"),
        ("Dutch", "nl", "NL"),
        ("Other (type code)", "", ""),
    ];
    let labels: Vec<&str> = choices.iter().map(|(l, _, _)| *l).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Pick a language")
        .items(&labels)
        .default(0)
        .interact()?;
    let (_, lang, region) = choices[idx];
    if lang.is_empty() {
        let raw: String = Input::with_theme(theme)
            .with_prompt("Locale (lang_REGION, e.g. ja_JP)")
            .interact_text()?;
        let mut parts = raw.splitn(2, &['_', '-'][..]);
        return Ok(Locale {
            lang: parts.next().unwrap_or("en").to_lowercase(),
            region: parts.next().unwrap_or("").to_uppercase(),
        });
    }
    Ok(Locale {
        lang: lang.into(),
        region: region.into(),
    })
}

// ---------------------------------------------------------------------------
// Whisper step
// ---------------------------------------------------------------------------

/// Pick a *wake-loop* Whisper model. We filter the catalog to the fast
/// end (tiny / base / small) — medium and large make no sense in an
/// always-on listener and would defeat the point of the override.
fn choose_wake_model(theme: &ColorfulTheme, defaults: &Defaults) -> Result<ModelInfo> {
    let mut catalog: Vec<ModelInfo> = whisper::catalog()
        .into_iter()
        .filter(|m| matches!(m.id, "tiny" | "base" | "small"))
        .filter(|m| !m.english_only || defaults.whisper_language == "en")
        .collect();
    catalog.sort_by_key(|m| !m.id.starts_with("base"));
    let labels: Vec<String> = catalog.iter().map(ModelInfo::label).collect();
    let default_idx = catalog
        .iter()
        .position(|m| m.id.starts_with("base"))
        .unwrap_or(0);
    let idx = Select::with_theme(theme)
        .with_prompt("Wake-loop model (will be downloaded to the same data dir)")
        .items(&labels)
        .default(default_idx)
        .interact()?;
    Ok(catalog[idx].clone())
}

fn choose_whisper_model(theme: &ColorfulTheme, defaults: &Defaults) -> Result<ModelInfo> {
    let want_english_only = defaults.whisper_language == "en"
        && Confirm::with_theme(theme)
            .with_prompt(
                "English-only? Picks faster .en checkpoints (≈30% speed-up, English-only).",
            )
            .default(false)
            .interact()?;

    let mut catalog: Vec<ModelInfo> = whisper::catalog()
        .into_iter()
        .filter(|m| m.english_only == want_english_only)
        .collect();
    // Put the recommended `base` (or `base.en`) first.
    catalog.sort_by_key(|m| !m.id.starts_with("base"));
    let labels: Vec<String> = catalog.iter().map(ModelInfo::label).collect();

    let default_idx = catalog
        .iter()
        .position(|m| m.id.starts_with("base"))
        .unwrap_or(0);
    let idx = Select::with_theme(theme)
        .with_prompt("Whisper model (will download to ~/.local/share/jarvis/whisper)")
        .items(&labels)
        .default(default_idx)
        .interact()?;
    Ok(catalog[idx].clone())
}

// ---------------------------------------------------------------------------
// Piper voice step
// ---------------------------------------------------------------------------

fn choose_piper_voice(theme: &ColorfulTheme, defaults: &Defaults) -> Result<String> {
    // The user might be offline (or HF down). Fetching the index is best
    // effort; on failure we fall back to the static default voice ID and
    // let the user override if they care.
    let voices = match voices::fetch_index() {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Could not fetch Piper voice index ({e}). Falling back to default voice {}.",
                defaults.piper_voice
            );
            return Ok(defaults.piper_voice.clone());
        }
    };

    let filtered = voices::filter_by_language(voices, &defaults.piper_lang_filter);
    if filtered.is_empty() {
        return Ok(defaults.piper_voice.clone());
    }

    let labels: Vec<String> = filtered.iter().map(Voice::label).collect();
    let default_idx = filtered
        .iter()
        .position(|v| v.key == defaults.piper_voice)
        .unwrap_or(0);

    let idx = Select::with_theme(theme)
        .with_prompt("Piper voice (downloads on first use)")
        .items(&labels)
        .default(default_idx)
        .interact()?;
    Ok(filtered[idx].key.clone())
}

// ---------------------------------------------------------------------------
// Wake-word step
// ---------------------------------------------------------------------------

fn configure_wake(theme: &ColorfulTheme, cfg: &mut JarvisConfig) -> Result<()> {
    println!(
        "Wake-word mode lets Jarvis listen continuously and trigger when you \
         say a phrase. If you'd rather just press a hotkey, leave it as \"none\"."
    );

    // Backends shown to the user. Stubs are labelled (roadmap) so picking
    // them is intentional — `wake::build` will error at daemon-start time
    // explaining how to enable each.
    let options = [
        ("none", "Hotkey only (recommended for first use)"),
        (
            "whisper",
            "whisper-cli phrase match — pick any words, reuses STT",
        ),
        ("sherpa", "sherpa-onnx KWS (roadmap — not yet implemented)"),
        ("openwakeword", "openwakeword pre-trained models (roadmap)"),
        (
            "rustpotter",
            "rustpotter, trained from your voice (roadmap)",
        ),
    ];
    let labels: Vec<String> = options
        .iter()
        .map(|(name, desc)| format!("{name:<14}  {desc}"))
        .collect();
    let default_idx = options
        .iter()
        .position(|(n, _)| *n == cfg.wake.backend.as_str())
        .unwrap_or(0);

    let idx = Select::with_theme(theme)
        .with_prompt("Wake backend")
        .items(&labels)
        .default(default_idx)
        .interact()?;
    let (chosen, _) = options[idx];
    cfg.wake.backend = chosen.into();

    if chosen == "none" {
        cfg.wake.enabled = false;
        cfg.wake.phrases = Vec::new();
        println!("  → wake disabled. Bind `jarvis listen` to a hotkey in your WM.");
        return Ok(());
    }

    if !wake_backends::is_implemented(chosen) {
        println!(
            "  ⚠ The {chosen:?} backend is on the roadmap but not implemented \
             yet. Your config will be saved, but `jarvis daemon` will refuse \
             to start with it until the backend lands."
        );
    }

    cfg.wake.enabled = Confirm::with_theme(theme)
        .with_prompt("Enable the wake-word daemon? (you can change this later)")
        .default(true)
        .interact()?;

    let default_phrase = if cfg.wake.phrases.is_empty() {
        "jarvis".to_string()
    } else {
        cfg.wake.phrases.join(", ")
    };
    let raw: String = Input::with_theme(theme)
        .with_prompt("Wake phrase(s), comma-separated (e.g. \"jarvis, mutombo\")")
        .default(default_phrase)
        .interact_text()?;
    let phrases: Vec<String> = raw
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if phrases.is_empty() {
        println!("  ⚠ No phrases given — falling back to [\"jarvis\"]");
        cfg.wake.phrases = vec!["jarvis".into()];
    } else {
        cfg.wake.phrases = phrases;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent step
// ---------------------------------------------------------------------------

fn configure_agent(theme: &ColorfulTheme, cfg: &mut JarvisConfig) -> Result<()> {
    let agents = [
        ("claude", "Claude Code CLI"),
        ("openai", "OpenAI / ChatGPT"),
        ("gemini", "Google Gemini"),
        ("warp", "Warp (oz CLI)"),
        ("shell", "Custom CLI (Ollama, llama.cpp, your own script)"),
    ];
    let labels: Vec<String> = agents.iter().map(|(k, v)| format!("{k:<8}  {v}")).collect();
    let current_idx = agents
        .iter()
        .position(|(k, _)| *k == cfg.agent.name.as_str())
        .unwrap_or(0);
    let idx = Select::with_theme(theme)
        .with_prompt("Which agent?")
        .items(&labels)
        .default(current_idx)
        .interact()?;
    let (name, _) = agents[idx];
    cfg.agent.name = name.into();

    // Per-agent follow-ups: API keys, command, model.
    match name {
        "openai" => prompt_api_key(theme, cfg, "OPENAI_API_KEY")?,
        "gemini" => prompt_api_key(theme, cfg, "GEMINI_API_KEY")?,
        "warp" => configure_warp_auth(theme, cfg)?,
        "shell" => prompt_shell_command(theme, cfg)?,
        _ => {}
    }
    Ok(())
}

/// Pick the right auth path for the warp/oz agent. `oz` authenticates
/// via `oz login` (token cached in its state dir) for nearly every
/// user; the `WARP_API_KEY` env var is the alternative for headless
/// / CI flows. The old prompt assumed key-based auth unconditionally,
/// which made the wizard ask for a key even when oz was already
/// logged in. Now we probe `oz whoami` first and skip the prompt
/// entirely when authenticated.
fn configure_warp_auth(theme: &ColorfulTheme, cfg: &mut JarvisConfig) -> Result<()> {
    if oz_is_authenticated() {
        println!("  ✓ `oz` is already logged in — no API key needed.");
        cfg.agent.options.remove("api_key");
        return Ok(());
    }
    println!("  ℹ Recommended: run `oz login` (no key plumbing needed).");
    println!("    Or store WARP_API_KEY below if you prefer key-based auth.");
    prompt_api_key(theme, cfg, "WARP_API_KEY")
}

/// `oz whoami` exits 0 only when the local oz install holds a valid
/// session. Cheap enough to call from the wizard (~couple of seconds
/// the one time it touches Warp's API), and unambiguous — no parsing
/// required, just the exit code.
fn oz_is_authenticated() -> bool {
    use std::process::Stdio;
    std::process::Command::new("oz")
        .arg("whoami")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn prompt_api_key(theme: &ColorfulTheme, cfg: &mut JarvisConfig, env_var: &str) -> Result<()> {
    if std::env::var(env_var)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        println!("  ✓ {env_var} is set in environment — leaving config api_key empty.");
        cfg.agent.options.remove("api_key");
        return Ok(());
    }
    let store_in_config = Confirm::with_theme(theme)
        .with_prompt(format!(
            "{env_var} is not set. Store the key in config.toml? (No = you'll export it in your shell)"
        ))
        .default(false)
        .interact()?;
    if !store_in_config {
        println!("  → Make sure to `export {env_var}=...` before running jarvis listen / daemon.");
        cfg.agent.options.remove("api_key");
        return Ok(());
    }
    let key: String = Password::with_theme(theme)
        .with_prompt(format!("{env_var} value"))
        .interact()?;
    cfg.agent
        .options
        .insert("api_key".into(), toml::Value::String(key));
    Ok(())
}

fn prompt_shell_command(theme: &ColorfulTheme, cfg: &mut JarvisConfig) -> Result<()> {
    let example = "ollama run llama3";
    let raw: String = Input::with_theme(theme)
        .with_prompt("Shell agent command (reads prompt on stdin, writes reply on stdout)")
        .default(example.into())
        .interact_text()?;
    // Naive whitespace split — sufficient for the common "ollama run X" /
    // "my-script --flag" cases. Users with shell-style quoting can edit the
    // resulting array in config.toml afterwards.
    let argv: Vec<toml::Value> = raw
        .split_whitespace()
        .map(|s| toml::Value::String(s.to_string()))
        .collect();
    cfg.agent
        .options
        .insert("command".into(), toml::Value::Array(argv));
    Ok(())
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// If the broken-to-load config file mentions `[dispatcher.fallback]`,
/// print a tip so the user knows to copy that block back from the
/// resulting `.bak` once the wizard finishes regenerating the file.
///
/// Exposed as a free function (and the substring helper below as
/// `pub(crate)`) so the integration tests can lock the trigger logic
/// without depending on dialoguer / a real config-load failure.
pub fn config_text_mentions_dispatcher_fallback(text: &str) -> bool {
    text.contains("[dispatcher.fallback")
}

fn warn_if_had_dispatcher_fallback(path: &Path) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    if config_text_mentions_dispatcher_fallback(&raw) {
        println!(
            "  ℹ Your broken config had a [dispatcher.fallback] section. \
             After the wizard regenerates the file you can paste that \
             block back from the .bak."
        );
        println!();
    }
}

/// Back up `path` to `path.bak` (rotating any existing .bak out of the way).
fn backup_existing(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bak = path.with_extension("toml.bak");
    if bak.exists() {
        // Rotate .bak → .bak.<unix-ts> so we never silently overwrite a prior
        // backup. The .bak slot always holds the most recent old config.
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let rotated = path.with_extension(format!("toml.bak.{ts}"));
        fs::rename(&bak, &rotated).with_context(|| {
            format!(
                "rotating old backup {} -> {}",
                bak.display(),
                rotated.display()
            )
        })?;
        println!("  rotated previous backup to {}", rotated.display());
    }
    fs::rename(path, &bak)
        .with_context(|| format!("backing up {} -> {}", path.display(), bak.display()))?;
    println!("  backed up old config to {}", bak.display());
    Ok(())
}

/// Render the config struct back to a TOML string.
///
/// Exposed (alongside `save_config`) so integration tests can assert the
/// round-trip without going through the filesystem. We re-render rather
/// than patch the existing file because the example ships with extensive
/// comments and round-tripping comments through serde would be
/// unreliable. The bundled example remains the canonical documentation;
/// the user's edited file is the working copy.
pub fn render_config(cfg: &JarvisConfig) -> Result<String> {
    let mut buf = String::new();
    buf.push_str("# Generated by `jarvis setup`. Edit freely.\n");
    buf.push_str(&format!("config_version = {}\n", cfg.config_version));
    buf.push_str(&format!("log_level = \"{}\"\n", cfg.log_level));
    buf.push_str(&format!("speak_responses = {}\n\n", cfg.speak_responses));

    serialize_section(&mut buf, "wake", &cfg.wake)?;
    serialize_section(&mut buf, "record", &cfg.record)?;
    serialize_section(&mut buf, "stt", &cfg.stt)?;
    serialize_section(&mut buf, "tts", &cfg.tts)?;
    serialize_agent(&mut buf, cfg)?;
    serialize_section(&mut buf, "session", &cfg.session)?;
    serialize_section(&mut buf, "tasks", &cfg.tasks)?;
    serialize_dispatcher_fallback(&mut buf, cfg)?;
    serialize_apps(&mut buf, cfg)?;
    Ok(buf)
}

fn save_config(cfg: &JarvisConfig, path: &Path) -> Result<()> {
    let buf = render_config(cfg)?;
    fs::write(path, buf).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Render `[dispatcher.fallback]` if configured. We wrap the inner
/// value in `{ dispatcher: { fallback: ... } }` so the toml crate
/// emits the dotted parent header `[dispatcher.fallback]` and renders
/// any sub-tables (like `headers`) with the full prefix. A bare
/// `serialize_section("dispatcher", ...)` would emit `[fallback]`
/// inside `[dispatcher]`, which round-trips wrong because the
/// `JarvisConfig` struct expects `dispatcher` to *be* the parent.
fn serialize_dispatcher_fallback(buf: &mut String, cfg: &JarvisConfig) -> Result<()> {
    let Some(fallback) = &cfg.dispatcher.fallback else {
        return Ok(());
    };
    let mut root = toml::Table::new();
    let mut dispatcher = toml::Table::new();
    dispatcher.insert("fallback".into(), fallback.clone());
    root.insert("dispatcher".into(), toml::Value::Table(dispatcher));
    let rendered = toml::to_string(&root)?;
    buf.push_str(&rendered);
    buf.push('\n');
    Ok(())
}

/// Render `[apps.aliases]` if the user has any entries. Empty
/// aliases skip the section entirely to keep the generated file
/// tidy — `AppsConfig::default()` round-trips through `config::load`
/// without needing a literal `[apps]` block.
fn serialize_apps(buf: &mut String, cfg: &JarvisConfig) -> Result<()> {
    if cfg.apps.aliases.is_empty() {
        return Ok(());
    }
    let mut root = toml::Table::new();
    let mut apps = toml::Table::new();
    let mut aliases = toml::Table::new();
    for (k, v) in &cfg.apps.aliases {
        aliases.insert(k.clone(), toml::Value::String(v.clone()));
    }
    apps.insert("aliases".into(), toml::Value::Table(aliases));
    root.insert("apps".into(), toml::Value::Table(apps));
    let rendered = toml::to_string(&root)?;
    buf.push_str(&rendered);
    buf.push('\n');
    Ok(())
}

fn serialize_section<T: serde::Serialize>(buf: &mut String, name: &str, value: &T) -> Result<()> {
    let raw = toml::to_string(value)?;
    buf.push_str(&format!("[{name}]\n"));
    buf.push_str(&raw);
    buf.push('\n');
    Ok(())
}

fn serialize_agent(buf: &mut String, cfg: &JarvisConfig) -> Result<()> {
    buf.push_str("[agent]\n");
    buf.push_str(&format!("name = \"{}\"\n", cfg.agent.name));
    for (k, v) in &cfg.agent.options {
        // Top-level value formatting; `toml` produces a temporary doc.
        let line = toml::to_string(&toml::Table::from_iter([(k.clone(), v.clone())]))?;
        buf.push_str(line.trim_end());
        buf.push('\n');
    }
    Ok(())
}

// Tiny helper so the JarvisConfig sub-structs become Serialize. They derive
// Deserialize today; the wizard needs Serialize too. Adding the derive in
// `config.rs` keeps the trait surface symmetric.
//
// (Implemented there — this comment exists to point future readers at the
// reason for the new `#[derive(Serialize)]`.)
