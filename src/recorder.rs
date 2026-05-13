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
        // `silenceremove` *does* support auto-termination on trailing
        // silence, but the parameter is named `stop_duration` — not
        // `stop_silence` (an earlier version of this code used the wrong
        // name and ffmpeg silently ignored it, leaving recordings to run
        // for the full `max_seconds`). When `stop_periods` is reached the
        // filter graph reports EOF and ffmpeg exits.
        "-af".into(),
        format!(
            "silenceremove=stop_periods=1:stop_duration={}:stop_threshold={}dB",
            cfg.silence_seconds, cfg.silence_threshold_db
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

/// Capture an utterance gated by a speech-onset window.
///
/// Unlike [`record_to_wav`], which gives the caller a single
/// hard-deadline recording, this opens the mic and:
///
/// 1. Waits up to `onset_window_secs` for the user to start speaking
///    (RMS sustained above `cfg.silence_threshold_db` for ~200 ms).
/// 2. Once speech starts, keeps recording until either trailing
///    silence of `cfg.silence_seconds` is detected or the utterance
///    hits `cfg.max_seconds` from onset.
///
/// Returns `Ok(None)` if the onset window elapses without speech (the
/// daemon's follow-up loop uses this to end the chain). Returns
/// `Ok(Some(path))` with a freshly-written WAV otherwise. The caller
/// owns the file and is expected to unlink it.
///
/// The audible-cue + 250 ms settle that `pipeline::run_turn` performs
/// for primary turns is intentionally skipped here — onset detection
/// is the cue. The leading buffer (samples observed *before* we are
/// sure it's speech) is preserved in the output so we don't clip
/// the first syllable, which is the exact bug that motivated this
/// function over a naive two-phase "detect then record" split.
pub fn record_with_onset(cfg: &RecordConfig, onset_window_secs: f32) -> Result<Option<PathBuf>> {
    // Same temp-file dance as record_to_wav.
    let tmp = tempfile::Builder::new()
        .prefix("jarvis-fu-")
        .suffix(".wav")
        .tempfile()?;
    let (_, out_path) = tmp.keep().context("persisting temp WAV")?;

    let mut child = spawn_raw_pcm_recorder()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("recorder stdout unavailable"))?;

    // 100 ms chunks at 16 kHz mono = 1600 samples = 3200 bytes. Matches
    // the wake backend's framing so the RMS/dB math behaves the same.
    const SAMPLE_RATE: u32 = 16_000;
    const CHUNK_MS: f32 = 100.0;
    const CHUNK_SAMPLES: usize = (SAMPLE_RATE as f32 * CHUNK_MS / 1000.0) as usize;
    const CHUNK_BYTES: usize = CHUNK_SAMPLES * 2;
    /// Always-on ring of the last N chunks of audio, regardless of
    /// whether they exceeded the voice threshold. When onset confirms,
    /// we prepend the ring to the capture so a soft consonant or a
    /// breathy syllable that started *just below* threshold isn't
    /// lost. Five chunks = 500 ms — generous enough to grab a typical
    /// gradual onset without polluting captures with audible noise.
    const PREROLL_CHUNKS: usize = 5;
    let chunks_per_sec = 1000.0 / CHUNK_MS;
    let onset_sustain_chunks = 2usize; // 200 ms — short enough to feel snappy

    let silence_threshold = cfg.silence_threshold_db;
    let silence_chunks_needed = ((cfg.silence_seconds * chunks_per_sec).ceil() as usize).max(1);
    let max_capture_chunks = ((cfg.max_seconds * chunks_per_sec).ceil() as usize).max(1);
    let max_onset_chunks = ((onset_window_secs * chunks_per_sec).ceil() as usize).max(1);

    enum State {
        SeekingOnset { voice_run: usize },
        Capturing { silence_run: usize, captured: usize },
    }
    let mut state = State::SeekingOnset { voice_run: 0 };
    let mut samples: Vec<i16> = Vec::with_capacity(CHUNK_SAMPLES * (max_capture_chunks + 4));
    let mut preroll: std::collections::VecDeque<Vec<i16>> =
        std::collections::VecDeque::with_capacity(PREROLL_CHUNKS);
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut elapsed_chunks = 0usize;

    let outcome: Result<Option<PathBuf>> = (|| loop {
        if !read_exact_or_eof(&mut stdout, &mut buf)? {
            // Recorder died (mic unplugged, ffmpeg crash). Treat as
            // "no speech" rather than failing the turn — the daemon
            // will return to wake gating and the next attempt will
            // surface the underlying error if it's persistent.
            return Ok(None);
        }
        elapsed_chunks += 1;
        let rms = compute_rms_normalised(&buf);
        let dbfs = rms_to_dbfs(rms);
        let voiced = dbfs >= silence_threshold;
        let chunk_samples = decode_chunk_to_i16(&buf);

        match state {
            State::SeekingOnset { mut voice_run } => {
                // Always feed the preroll ring, voiced or not. When
                // onset triggers we flush the ring into `samples` so
                // the leading edge survives, including any soft onset
                // chunks that didn't individually cross threshold.
                if preroll.len() == PREROLL_CHUNKS {
                    preroll.pop_front();
                }
                preroll.push_back(chunk_samples);

                if voiced {
                    voice_run += 1;
                    if voice_run >= onset_sustain_chunks {
                        // Flush the preroll into `samples` before
                        // transitioning so the captured WAV starts
                        // ~500 ms before the confirmed-voice moment.
                        for c in preroll.drain(..) {
                            samples.extend(c);
                        }
                        state = State::Capturing {
                            silence_run: 0,
                            captured: voice_run,
                        };
                    } else {
                        state = State::SeekingOnset { voice_run };
                    }
                } else {
                    // Drop voice_run on a not-voiced gap. The preroll
                    // is still maintained above, so a real onset that
                    // follows a brief dip still gets its leading
                    // audio when it triggers.
                    state = State::SeekingOnset { voice_run: 0 };
                    if elapsed_chunks >= max_onset_chunks {
                        return Ok(None);
                    }
                }
            }
            State::Capturing {
                mut silence_run,
                mut captured,
            } => {
                samples.extend(chunk_samples);
                captured += 1;
                if voiced {
                    silence_run = 0;
                } else {
                    silence_run += 1;
                    if silence_run >= silence_chunks_needed {
                        return Ok(Some(out_path.clone()));
                    }
                }
                if captured >= max_capture_chunks {
                    return Ok(Some(out_path.clone()));
                }
                state = State::Capturing {
                    silence_run,
                    captured,
                };
            }
        }
    })();

    // SIGTERM the recorder either way: we have the data we need.
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let _ = child.wait();

    match outcome {
        Ok(Some(path)) => {
            write_pcm_wav(&path, SAMPLE_RATE, &samples)?;
            Ok(Some(path))
        }
        Ok(None) => {
            // Onset window expired; clean up the empty temp file.
            let _ = std::fs::remove_file(&out_path);
            Ok(None)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            Err(e)
        }
    }
}

/// Spawn a recorder writing raw 16-bit mono PCM at 16 kHz to stdout. The
/// caller is expected to SIGTERM the child when finished. We prefer
/// ffmpeg (clean raw output) then parecord / arecord. Mirrors the wake
/// backend's setup so behaviour is consistent across the codebase.
fn spawn_raw_pcm_recorder() -> Result<std::process::Child> {
    if which::which("ffmpeg").is_ok() {
        return Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "pulse",
                "-i",
                "default",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-f",
                "s16le",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning ffmpeg for onset-gated recording");
    }
    if which::which("parecord").is_ok() {
        return Command::new("parecord")
            .args(["--raw", "--format=s16le", "--rate=16000", "--channels=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning parecord for onset-gated recording");
    }
    if which::which("arecord").is_ok() {
        return Command::new("arecord")
            .args(["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning arecord for onset-gated recording");
    }
    Err(anyhow!(
        "no continuous-capable recorder found on PATH \
         (need one of: ffmpeg, parecord, arecord)"
    ))
}

fn read_exact_or_eof<R: std::io::Read>(r: &mut R, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => return Ok(false),
            n => filled += n,
        }
    }
    Ok(true)
}

fn compute_rms_normalised(bytes: &[u8]) -> f32 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut sum_sq = 0.0f64;
    let mut count = 0usize;
    for pair in bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]) as f64;
        sum_sq += sample * sample;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    let mean = sum_sq / count as f64;
    (mean.sqrt() / i16::MAX as f64) as f32
}

/// Convert a normalised RMS (0.0 = silence, 1.0 = full scale) to dBFS.
/// `silence_threshold_db` is expressed in dBFS so the call site
/// compares apples to apples. Returns `f32::NEG_INFINITY` for the
/// silent-buffer case so it's always strictly below any real threshold.
fn rms_to_dbfs(rms: f32) -> f32 {
    if rms <= 0.0 {
        return f32::NEG_INFINITY;
    }
    20.0 * rms.log10()
}

fn decode_chunk_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]))
        .collect()
}

fn write_pcm_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let data_size = (samples.len() * 2) as u32;
    let chunk_size = 36 + data_size;
    let byte_rate = sample_rate * 2; // 1 channel * 2 bytes/sample
    f.write_all(b"RIFF")?;
    f.write_all(&chunk_size.to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Calibration check for the silence threshold: a full-scale i16
    /// signal should land at 0 dBFS, half-scale at -6 dBFS, and so on.
    /// The follow-up onset gate compares `rms_to_dbfs(rms)` against
    /// `cfg.silence_threshold_db`; if this conversion drifts, the
    /// onset detector silently mis-triggers or under-triggers.
    #[test]
    fn rms_to_dbfs_calibration() {
        // Two bytes per sample, little-endian. A buffer of pure max-int
        // samples should produce an RMS very close to 1.0 (full scale).
        let full_scale: Vec<u8> = (0..1600).flat_map(|_| i16::MAX.to_le_bytes()).collect();
        let rms = compute_rms_normalised(&full_scale);
        assert!(rms > 0.99, "expected full-scale rms ~1.0, got {rms}");
        let db = rms_to_dbfs(rms);
        assert!(db.abs() < 0.5, "expected ~0 dBFS, got {db}");

        // Silence: all zero bytes. RMS is 0, conversion must yield
        // -infinity so it's below any sane threshold.
        let silence = vec![0u8; 3200];
        let rms = compute_rms_normalised(&silence);
        assert_eq!(rms, 0.0);
        assert_eq!(rms_to_dbfs(rms), f32::NEG_INFINITY);
    }

    /// The user-facing default must be loose enough that natural
    /// speech (including inter-word articulation dips at ~-38 dBFS)
    /// stays above threshold. Live testing on 2026-05-13 walked this
    /// down through -30 → -35 → -40 as we learned more about real
    /// USB-headset behavior. The contract here is "no stricter than
    /// -35 and no looser than -50" — anything outside is almost
    /// certainly a regression, but the band is wide enough that
    /// future tuning between -38 and -45 doesn't trip CI.
    #[test]
    fn silence_threshold_default_is_user_friendly() {
        let cfg = RecordConfig::default();
        assert!(
            cfg.silence_threshold_db >= -50.0 && cfg.silence_threshold_db <= -35.0,
            "expected default silence_threshold_db in [-50, -35], got {}",
            cfg.silence_threshold_db
        );
    }
}
