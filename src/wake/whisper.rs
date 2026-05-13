//! Whisper-as-wake-word: stream the mic through a cheap energy VAD, run
//! whisper.cpp on each detected speech segment, and trigger when the
//! transcript contains any of the configured phrases.
//!
//! Why this design:
//! * **Zero new deps.** We already ship whisper.cpp for the main STT flow.
//! * **Custom phrases for free.** `phrases = ["mutombo", "hola jarvis"]`
//!   works out of the box — no training, no Colab.
//! * **Cheap idle.** Energy VAD on 16 kHz mono PCM is ~0.1 % of a core.
//!   Whisper only runs when speech is detected.
//!
//! Tradeoff: latency. From "user finished saying the wake word" to "wake
//! fires" is roughly `silence_seconds + whisper_decode_time`, typically
//! 400–900 ms on CPU and 200–400 ms with a GPU build. For a wake word
//! that's acceptable; for press-to-talk it would not be.

use std::io::Read;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, anyhow};
use tracing::{debug, info, warn};

use super::WakeBackend;
use crate::config::{SttConfig, WakeConfig};
use crate::stt::{Stt, WhisperCli};

const SAMPLE_RATE: u32 = 16_000;
const BYTES_PER_SAMPLE: usize = 2; // i16 mono
/// Audio chunk size we read from the recorder at a time (~100 ms).
const CHUNK_SAMPLES: usize = (SAMPLE_RATE as usize) / 10;
const CHUNK_BYTES: usize = CHUNK_SAMPLES * BYTES_PER_SAMPLE;

pub struct WhisperWake {
    cfg: WakeConfig,
    /// We instantiate a dedicated `WhisperCli` for the wake loop so it can
    /// use a smaller/faster model than the main STT step if the user
    /// configures one (`[wake].stt_model_override`). For v1 we just reuse
    /// the main STT config.
    stt: WhisperCli,
    /// Phrases normalised once at construction so the hot path does string
    /// matching against pre-lowercased patterns.
    phrases_normalised: Vec<String>,
}

impl WhisperWake {
    pub fn new(cfg: WakeConfig) -> Result<Self> {
        if cfg.phrases.is_empty() {
            return Err(anyhow!(
                "wake backend = \"whisper\" requires [wake].phrases to be non-empty (e.g. phrases = [\"jarvis\", \"mutombo\"])"
            ));
        }

        // The wake-word path reuses whisper.cpp but with a copy of the STT
        // config — we don't share state, only the binary + model. The user
        // can later add `[wake].stt_model` for a smaller faster model.
        let stt_cfg = SttConfig::default(); // overridden by load_config in practice
        let stt = WhisperCli::new(stt_cfg);

        let phrases_normalised = cfg
            .phrases
            .iter()
            .map(|p| normalise(p))
            .filter(|p| !p.is_empty())
            .collect();

        Ok(Self {
            cfg,
            stt,
            phrases_normalised,
        })
    }
}

impl WakeBackend for WhisperWake {
    fn name(&self) -> &'static str {
        "whisper"
    }

    fn run(&self, on_wake: &mut dyn FnMut(), should_stop: &dyn Fn() -> bool) -> Result<()> {
        info!(
            phrases = ?self.cfg.phrases,
            threshold_rms = self.cfg.vad_rms_threshold,
            "wake/whisper listener starting"
        );

        let mut recorder = spawn_continuous_recorder(&self.cfg)?;
        let mut stdout = recorder
            .stdout
            .take()
            .ok_or_else(|| anyhow!("continuous recorder did not expose stdout"))?;

        // Per-segment buffer. Pre-allocated to ``max_listen_seconds`` worth
        // so the hot loop never reallocates.
        let max_samples = (self.cfg.max_listen_seconds * SAMPLE_RATE as f32) as usize;
        let mut segment: Vec<i16> = Vec::with_capacity(max_samples);
        let mut chunk_bytes = vec![0u8; CHUNK_BYTES];

        let silence_chunks_threshold = ((self.cfg.silence_seconds * 1000.0) as usize) / 100; // 100ms per chunk
        let mut silence_run = 0usize;
        let mut in_speech = false;

        let result: Result<()> = (|| {
            loop {
                if should_stop() {
                    info!("stop signal received; wake listener exiting");
                    break;
                }

                // Blocking read; will return Err on broken pipe (recorder died).
                let read_ok = read_exact_or_eof(&mut stdout, &mut chunk_bytes)?;
                if !read_ok {
                    return Err(anyhow!("recorder pipe closed unexpectedly"));
                }

                let rms = compute_rms(&chunk_bytes);
                let speaking = rms >= self.cfg.vad_rms_threshold;

                if speaking {
                    silence_run = 0;
                    in_speech = true;
                    append_i16_samples(&mut segment, &chunk_bytes, max_samples);
                } else if in_speech {
                    silence_run += 1;
                    append_i16_samples(&mut segment, &chunk_bytes, max_samples);
                    if silence_run >= silence_chunks_threshold || segment.len() >= max_samples {
                        // End of utterance — transcribe and check.
                        let matched = self.check_segment(&segment)?;
                        segment.clear();
                        silence_run = 0;
                        in_speech = false;
                        if matched {
                            info!("wake phrase detected; invoking callback");
                            on_wake();
                            // Briefly drain the pipe so the recorder buffer
                            // doesn't replay the user's own command into the
                            // next cycle.
                            drain_for_ms(&mut stdout, &mut chunk_bytes, 500)?;
                        }
                    }
                }
                // Else: silence with no preceding speech — nothing to do.
            }
            Ok(())
        })();

        let _ = recorder.kill();
        let _ = recorder.wait();
        result
    }
}

impl WhisperWake {
    fn check_segment(&self, samples: &[i16]) -> Result<bool> {
        if samples.is_empty() {
            return Ok(false);
        }
        // whisper needs a WAV file — write the buffer to a temp file, call
        // whisper-cli, then discard. This is more allocations than ideal but
        // keeps the integration honest (no in-process whisper bindings).
        let wav = tempfile::Builder::new()
            .prefix("jarvis-wake-")
            .suffix(".wav")
            .tempfile()?;
        let (_, path) = wav.keep().context("persisting wake WAV")?;
        write_wav(&path, samples)?;

        let result = (|| -> Result<bool> {
            let transcript = self.stt.transcribe(&path)?;
            let hay = normalise(&transcript);
            debug!(transcript = %transcript, "wake whisper transcribed segment");
            for phrase in &self.phrases_normalised {
                if hay.contains(phrase) {
                    return Ok(true);
                }
            }
            Ok(false)
        })();

        let _ = std::fs::remove_file(&path);
        result
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Lowercase + strip accents and surrounding whitespace so "Hola Jarvis!" and
/// "hola jarvís" both match a configured phrase "hola jarvis".
fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn compute_rms(bytes: &[u8]) -> f32 {
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

fn append_i16_samples(dst: &mut Vec<i16>, src_bytes: &[u8], cap_samples: usize) {
    for pair in src_bytes.chunks_exact(2) {
        if dst.len() >= cap_samples {
            break;
        }
        dst.push(i16::from_le_bytes([pair[0], pair[1]]));
    }
}

fn write_wav(path: &std::path::Path, samples: &[i16]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let data_size = (samples.len() * 2) as u32;
    let chunk_size = 36 + data_size;
    let byte_rate = SAMPLE_RATE * 2; // 1 channel * 2 bytes/sample
    f.write_all(b"RIFF")?;
    f.write_all(&chunk_size.to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // audio format = PCM
    f.write_all(&1u16.to_le_bytes())?; // channels
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

/// Spawn a recorder writing raw 16-bit mono PCM to stdout indefinitely.
/// We prefer `ffmpeg` (writes raw PCM cleanly) then `parecord`/`pw-record`.
fn spawn_continuous_recorder(_wake: &WakeConfig) -> Result<Child> {
    // We *don't* use the user's [record] backend choice here because the wake
    // listener needs an indefinite stream of raw PCM, while [record] is
    // optimised for a one-shot WAV-to-disk capture. Hard-coded preference
    // order: ffmpeg → parecord → pw-record (arecord can stream too but is
    // last because of its quirks with WAV-vs-raw on stdout).
    let device_arg_pulse = "default";
    if which::which("ffmpeg").is_ok() {
        return Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "pulse",
                "-i",
                device_arg_pulse,
                "-ac",
                "1",
                "-ar",
                &SAMPLE_RATE.to_string(),
                "-f",
                "s16le",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawning ffmpeg for continuous recording");
    }
    if which::which("parecord").is_ok() {
        return Command::new("parecord")
            .args(["--raw", "--format=s16le", "--rate=16000", "--channels=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawning parecord for continuous recording");
    }
    if which::which("arecord").is_ok() {
        return Command::new("arecord")
            .args(["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawning arecord for continuous recording");
    }
    Err(anyhow!(
        "no continuous-capable recorder found on PATH \
         (need one of: ffmpeg, parecord, arecord)"
    ))
}

/// Read exactly `buf.len()` bytes. Returns `Ok(false)` if EOF is hit before
/// the buffer fills, `Ok(true)` on success, `Err` on I/O failure.
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => return Ok(false),
            n => filled += n,
        }
    }
    Ok(true)
}

/// Read and discard ~`ms` milliseconds of audio. Used after a wake event to
/// keep the user's spoken command from being replayed into the next cycle.
fn drain_for_ms<R: Read>(r: &mut R, buf: &mut [u8], ms: u64) -> Result<()> {
    let chunks_to_drop = (ms as usize) / 100; // 100 ms per chunk
    for _ in 0..chunks_to_drop {
        if !read_exact_or_eof(r, buf)? {
            // Recorder died; the outer loop will pick it up on next read.
            warn!("recorder closed during post-wake drain");
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_strips_accents_and_case() {
        assert_eq!(normalise("Hola Jarvís"), "hola jarvis");
        assert_eq!(normalise("¿Mutombo?"), "mutombo");
        assert_eq!(normalise("MUTOMBO!  "), "mutombo");
    }

    #[test]
    fn rms_zero_for_silence() {
        let silence = vec![0u8; 320];
        assert!(compute_rms(&silence) < 1e-6);
    }

    #[test]
    fn rms_positive_for_signal() {
        // i16::MAX/2 little-endian repeated -> non-trivial RMS
        let half = (i16::MAX / 2).to_le_bytes();
        let buf: Vec<u8> = (0..160).flat_map(|_| half.iter().copied()).collect();
        assert!(compute_rms(&buf) > 0.4);
    }
}
