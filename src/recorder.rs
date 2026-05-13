//! Audio capture via external recorders (arecord / parecord / pw-record /
//! ffmpeg / user command).
//!
//! Jarvis itself never opens an audio device. Capturing audio is something
//! every distro already ships a binary for, and reusing that binary keeps us
//! free of CPAL / PortAudio linkage and works the same on Linux, macOS, and
//! BSD as long as the user has *some* recorder on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use tempfile::NamedTempFile;
use tracing::{debug, info};

use crate::config::RecordConfig;

/// Backends we know how to invoke without user-supplied command lines.
const KNOWN_BACKENDS: &[&str] = &["parecord", "pw-record", "arecord", "ffmpeg"];

fn detect() -> Result<&'static str> {
    for &name in KNOWN_BACKENDS {
        if which::which(name).is_ok() {
            return Ok(name);
        }
    }
    Err(anyhow!(
        "no supported recorder found in PATH. Install one of: alsa-utils \
         (arecord), pipewire-pulse (parecord), pipewire (pw-record), or ffmpeg"
    ))
}

fn build_arecord(cfg: &RecordConfig, out: &Path) -> Vec<String> {
    let mut cmd = vec![
        "arecord".into(),
        "-q".into(),
        "-t".into(),
        "wav".into(),
        "-f".into(),
        "S16_LE".into(),
        "-r".into(),
        cfg.sample_rate.to_string(),
        "-c".into(),
        cfg.channels.to_string(),
        "--max-file-time".into(),
        (cfg.max_seconds as u32).to_string(),
    ];
    if let Some(dev) = &cfg.device {
        cmd.extend(["-D".into(), dev.clone()]);
    }
    cmd.push(out.to_string_lossy().into_owned());
    cmd
}

fn build_parecord(cfg: &RecordConfig, out: &Path) -> Vec<String> {
    let mut cmd = vec![
        "parecord".into(),
        "--file-format=wav".into(),
        format!("--rate={}", cfg.sample_rate),
        format!("--channels={}", cfg.channels),
        "--format=s16le".into(),
    ];
    if let Some(dev) = &cfg.device {
        cmd.push(format!("--device={dev}"));
    }
    cmd.push(out.to_string_lossy().into_owned());
    cmd
}

fn build_pw_record(cfg: &RecordConfig, out: &Path) -> Vec<String> {
    let mut cmd = vec![
        "pw-record".into(),
        format!("--rate={}", cfg.sample_rate),
        format!("--channels={}", cfg.channels),
        "--format=s16".into(),
    ];
    if let Some(dev) = &cfg.device {
        cmd.push(format!("--target={dev}"));
    }
    cmd.push(out.to_string_lossy().into_owned());
    cmd
}

fn build_ffmpeg(cfg: &RecordConfig, out: &Path) -> Vec<String> {
    // PulseAudio input works under PipeWire too (pipewire-pulse). On macOS
    // ffmpeg supports `-f avfoundation -i :0`; users hit that via the
    // `command` backend so we don't have to OS-detect here.
    let device = cfg.device.clone().unwrap_or_else(|| "default".into());
    vec![
        "ffmpeg".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "pulse".into(),
        "-i".into(),
        device,
        "-ac".into(),
        cfg.channels.to_string(),
        "-ar".into(),
        cfg.sample_rate.to_string(),
        "-af".into(),
        format!(
            "silenceremove=stop_periods=1:stop_silence={}:stop_threshold=-40dB",
            cfg.silence_seconds
        ),
        "-t".into(),
        cfg.max_seconds.to_string(),
        "-y".into(),
        out.to_string_lossy().into_owned(),
    ]
}

fn build_command(cfg: &RecordConfig, out: &Path) -> Result<Vec<String>> {
    let backend = if cfg.backend == "auto" {
        detect()?.to_string()
    } else {
        cfg.backend.clone()
    };

    match backend.as_str() {
        "arecord" => Ok(build_arecord(cfg, out)),
        "parecord" => Ok(build_parecord(cfg, out)),
        "pw-record" => Ok(build_pw_record(cfg, out)),
        "ffmpeg" => Ok(build_ffmpeg(cfg, out)),
        "command" => {
            if cfg.command.is_empty() {
                return Err(anyhow!(
                    "recorder backend='command' but record.command is empty"
                ));
            }
            Ok(cfg
                .command
                .iter()
                .map(|arg| arg.replace("{out}", &out.to_string_lossy()))
                .collect())
        }
        other => Err(anyhow!("unknown recorder backend: {other:?}")),
    }
}

/// Record one utterance to a freshly-created temp WAV and return its path.
///
/// The temp file is created with `delete = false` so it survives the
/// `NamedTempFile` going out of scope; callers are expected to unlink it
/// after they're done with the audio (the pipeline does this).
pub fn record_to_wav(cfg: &RecordConfig) -> Result<PathBuf> {
    let tmp = NamedTempFile::with_prefix("jarvis-")?;
    let (_, out_path) = tmp.keep().context("persisting temp WAV")?;

    let argv = build_command(cfg, &out_path)?;
    info!(
        recorder = %argv[0],
        output = %out_path.display(),
        "recording"
    );
    debug!("argv: {argv:?}");

    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawning {}", argv[0]))?;

    if !status.success() {
        return Err(anyhow!(
            "recorder {:?} exited with status {status}",
            argv[0]
        ));
    }
    Ok(out_path)
}

/// Play a WAV file using whichever player is on PATH.
pub fn play_wav(path: &Path) -> Result<()> {
    for player in ["paplay", "pw-play", "aplay", "afplay"] {
        if which::which(player).is_ok() {
            let status = Command::new(player)
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .with_context(|| format!("spawning {player}"))?;
            if status.success() {
                return Ok(());
            }
        }
    }
    Err(anyhow!(
        "no audio player found (need paplay, pw-play, aplay, or afplay on macOS)"
    ))
}
