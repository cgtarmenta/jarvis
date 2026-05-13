//! Speech-to-text: spawn a transcription binary, return the printed text.
//!
//! The default backend wraps `whisper-cli` from whisper.cpp. The `command`
//! backend pipes a WAV path into any user-supplied CLI (Voxtype, a remote
//! transcription server, an agent-generated plugin, …).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use tracing::warn;

use crate::config::SttConfig;

pub trait Stt {
    fn transcribe(&self, wav: &Path) -> Result<String>;
}

/// Wraps whisper.cpp's `whisper-cli`.
pub struct WhisperCli {
    cfg: SttConfig,
}

impl WhisperCli {
    pub fn new(cfg: SttConfig) -> Self {
        if which::which(&cfg.binary).is_err() {
            warn!(
                binary = %cfg.binary,
                "STT binary not found in PATH — install whisper.cpp (Arch: pacman -S whisper.cpp, macOS: brew install whisper-cpp)"
            );
        }
        if !Path::new(&cfg.model).is_file() {
            warn!(
                model = %cfg.model,
                "whisper model not found — point [stt].model at a ggml-*.bin file"
            );
        }
        Self { cfg }
    }
}

impl Stt for WhisperCli {
    fn transcribe(&self, wav: &Path) -> Result<String> {
        let mut cmd = Command::new(&self.cfg.binary);
        cmd.args(["-m", &self.cfg.model])
            .args(["-f", &wav.to_string_lossy()])
            .args(["-l", &self.cfg.language])
            .args(["-t", &self.cfg.threads.to_string()])
            .args(["--no-timestamps", "--no-prints"]);

        // GPU controls. `use_gpu = None` is the default; we add nothing and
        // whisper.cpp uses its compiled-in default (GPU when available).
        if self.cfg.use_gpu == Some(false) {
            cmd.arg("--no-gpu");
        }
        if let Some(dev) = self.cfg.gpu_device {
            cmd.args(["-d", &dev.to_string()]);
        }
        if self.cfg.flash_attn {
            cmd.arg("--flash-attn");
        }

        for arg in &self.cfg.extra_args {
            cmd.arg(arg);
        }

        let out = cmd
            .output()
            .with_context(|| format!("spawning {}", self.cfg.binary))?;
        if !out.status.success() {
            return Err(anyhow!(
                "{} exited with {}: {}",
                self.cfg.binary,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        // whisper-cli prints each segment on its own line. Collapse + trim
        // so the agent prompt sees a single clean line.
        let text = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(text)
    }
}

/// User-supplied transcription command. `{wav}` is replaced with the WAV path.
pub struct CommandStt {
    cfg: SttConfig,
}

impl CommandStt {
    pub fn new(cfg: SttConfig) -> Result<Self> {
        if cfg.command.is_empty() {
            return Err(anyhow!("stt.backend=\"command\" requires stt.command"));
        }
        Ok(Self { cfg })
    }
}

impl Stt for CommandStt {
    fn transcribe(&self, wav: &Path) -> Result<String> {
        let wav_str = wav.to_string_lossy();
        let argv: Vec<String> = self
            .cfg
            .command
            .iter()
            .map(|a| a.replace("{wav}", &wav_str))
            .collect();

        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .with_context(|| format!("spawning {}", argv[0]))?;
        if !out.status.success() {
            return Err(anyhow!(
                "stt command {:?} exited with {}: {}",
                argv[0],
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

pub fn build(cfg: SttConfig) -> Result<Box<dyn Stt + Send + Sync>> {
    match cfg.backend.to_lowercase().as_str() {
        "whisper-cli" | "whisper" | "whisper.cpp" => Ok(Box::new(WhisperCli::new(cfg))),
        "command" => Ok(Box::new(CommandStt::new(cfg)?)),
        other => Err(anyhow!("unsupported STT backend: {other:?}")),
    }
}
