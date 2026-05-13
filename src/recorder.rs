//! Audio capture via external recorders (arecord / parecord / pw-record /
//! ffmpeg / user command).
//!
//! Jarvis itself never opens an audio device. Capturing audio is something
//! every distro already ships a binary for, and reusing that binary keeps us
//! free of CPAL / PortAudio linkage and works the same on Linux, macOS, and
//! BSD as long as the user has *some* recorder on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tracing::{debug, info};

use crate::config::RecordConfig;

/// Backends we know how to invoke. Order is **deliberate** — `ffmpeg` is
/// first because it's the only one with built-in silence-based auto-stop
/// (`silenceremove`); without it the recorder runs to the full `max_seconds`
/// and a voice turn always feels slow. arecord comes next because it honours
/// `--max-file-time`; parecord/pw-record are last because they have no
/// duration flag at all and rely on our timeout-kill fallback.
const KNOWN_BACKENDS: &[&str] = &["ffmpeg", "arecord", "parecord", "pw-record"];

fn detect() -> Result<&'static str> {
    for &name in KNOWN_BACKENDS {
        if which::which(name).is_ok() {
            return Ok(name);
        }
    }
    Err(anyhow!(
        "no supported recorder found in PATH. Install one of: ffmpeg, \
         alsa-utils (arecord), pipewire-pulse (parecord), pipewire (pw-record)"
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
///
/// We spawn a watchdog thread that sends `SIGTERM` after `cfg.max_seconds`,
/// because parecord/pw-record have no built-in duration limit and would
/// otherwise record forever. The +0.5 s slack lets backends that *do*
/// honour their own duration flag finish writing the WAV header cleanly
/// before we step in.
pub fn record_to_wav(cfg: &RecordConfig) -> Result<PathBuf> {
    // The `.wav` suffix matters: ffmpeg infers the output muxer from the
    // file extension. Without it, ffmpeg refuses to record with
    // "Unable to choose an output format". arecord/parecord don't care but
    // it costs nothing to give them a real extension too.
    let tmp = tempfile::Builder::new()
        .prefix("jarvis-")
        .suffix(".wav")
        .tempfile()?;
    let (_, out_path) = tmp.keep().context("persisting temp WAV")?;

    let argv = build_command(cfg, &out_path)?;
    info!(
        recorder = %argv[0],
        output = %out_path.display(),
        "recording"
    );
    debug!("argv: {argv:?}");

    // Stderr is shown so the user sees ffmpeg / arecord status — handy when
    // debugging "why isn't it hearing me?".
    eprintln!(
        "🎤 Recording (up to {:.0}s, Ctrl-C to stop)...",
        cfg.max_seconds
    );

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {}", argv[0]))?;

    let pid = child.id();
    let timeout = Duration::from_secs_f32(cfg.max_seconds + 0.5);
    let finished = Arc::new(AtomicBool::new(false));
    let finished_for_timer = Arc::clone(&finished);
    let watchdog = thread::spawn(move || {
        // Sleep then send SIGTERM if the process hasn't already exited.
        let step = Duration::from_millis(100);
        let mut waited = Duration::ZERO;
        while waited < timeout {
            if finished_for_timer.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(step);
            waited += step;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    });

    let status = child.wait()?;
    finished.store(true, Ordering::Relaxed);
    let _ = watchdog.join();

    // `success()` is false when we SIGTERM the child, but the WAV is still
    // playable. Treat any non-zero exit as a soft failure: log it and let
    // the STT step decide whether it could parse audio out of the result.
    if !status.success() {
        debug!(
            recorder = %argv[0],
            exit = ?status.code(),
            "recorder exited non-zero (expected when timeout-killed)"
        );
    }
    eprintln!("🎤 Done.");
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
