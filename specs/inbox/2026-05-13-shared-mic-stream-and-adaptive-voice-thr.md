---
id:
title: Shared mic stream and adaptive voice thresholds
status: inbox
owner: unassigned
created: 2026-05-13
shipped:
verifying:
related:
---

# Shared mic stream and adaptive voice thresholds

## Why

Live testing on 2026-05-13 surfaced two coupled problems with the
spec 0007 follow-up recorder that can't be fixed by tuning constants:

1. **Concurrent recorders fight over the mic.** Today the wake
   backend keeps an `ffmpeg → pulse default` continuous recorder
   alive across the daemon's lifetime, *and* the follow-up turn
   spawns its own recorder against the same pulse source. We
   went through three workarounds (ffmpeg → ffmpeg with looser
   thresholds → parecord) and each fixed one symptom at the cost
   of another. Final state with parecord: the follow-up registers
   as a separate pulse application, gets its own per-client
   volume slider that Pulse remembers across runs, and the user
   has to manually raise that slider every time pulse forgets it.
   First-syllable / last-syllable drops kept appearing because
   the two captures don't see the same audio.

2. **Static thresholds can't fit every mic / room / speaker.**
   The default `silence_threshold_db` walked through -30, -35,
   -40 over a single afternoon, each time miscalibrated for the
   *next* user environment. Even at -40 the user's inter-word
   articulation dips and his lowest-amplitude consonants
   sometimes fell below threshold; raising silence_seconds to
   4 s helped but introduced sluggish turn-end on truly-finished
   utterances. The actual signal we want — "this user's voice
   vs. this room's background" — is something we could *measure*
   automatically rather than configure.

Solving both is what makes the conversational loop feel
predictable instead of magical-when-it-works.

## What

- [ ] Single audio capture process owns the mic for the whole
      daemon lifetime. Wake VAD, follow-up onset detection, and
      utterance recording all consume from one shared raw-PCM
      stream — no second recorder is ever spawned. Removes the
      class of bugs caused by concurrent pulse clients (volume
      sliders, dts conflicts, divergent buffers).
- [ ] Daemon startup runs a one-shot calibration: ~3 s of ambient
      capture to measure the noise floor (median + p95 RMS in
      dBFS). The threshold defaults are computed relative to
      that floor (e.g. `floor + 8 dB` for "voice present") rather
      than hard-coded numbers. Stored in
      `~/.cache/jarvis/voice-profile.json` so re-launches don't
      re-calibrate.
- [ ] Per-utterance feedback updates the profile: when an
      utterance gets transcribed non-empty, record its peak and
      median RMS. The profile learns "this user's voice band"
      over time and the threshold adapts (with bounds so a
      noisy event can't poison it).
- [ ] `jarvis doctor` reports the current voice profile (noise
      floor, voice band, effective threshold) so users can sanity
      check what the system thinks of their mic without reading
      debug logs.
- [ ] `jarvis voice-calibrate` (or similar) re-runs the
      calibration on demand for users who change mics / rooms.
- [ ] Tests cover: the calibration math (synthetic samples →
      expected floor); the profile load/save round-trip; the
      threshold-derivation function (floor + offset =
      threshold).

## How

Architecture sketch (not binding):

- New module `src/audio_bus.rs` (name negotiable) owns the
  single recorder process and exposes a chunk-broadcast
  channel. Consumers (`wake::*`, `recorder::record_with_onset`)
  subscribe and receive 100 ms PCM chunks. The wake backend's
  current `spawn_continuous_recorder` becomes obsolete — moved
  into the bus.
- The follow-up recorder no longer spawns parecord/ffmpeg; it
  just subscribes to the bus and runs the same RMS/state
  machine on chunks it pulls. The dts conflict disappears
  because there is only one pulse client.
- `src/voice_profile.rs` holds the calibration. Schema is a
  small JSON: `{ noise_floor_dbfs, voice_p50_dbfs,
  voice_p95_dbfs, samples_observed }`. Updated atomically.
- Threshold derivation lives in one function so the math is
  testable. Bounds protect against single noisy utterances
  pushing the floor too high or speech too low.
- `RecordConfig::silence_threshold_db` becomes an *override*
  — if set explicitly in config, it wins; otherwise the
  profile-derived value is used. Backwards compatible.

Tradeoffs to be aware of:

- The bus introduces a single point of failure (one process
  death = both wake and follow-up die). Mitigated by a
  restart watchdog with backoff.
- Calibration on startup adds ~3 s to daemon ready time.
  Acceptable because it pays back the saved tuning time per
  session.
- Profile drift: if the user gets a cold and their voice
  drops 6 dB, the profile will adapt slowly. A
  `jarvis voice-calibrate` reset gets them back fast.

## Journal

- 2026-05-13: opened. Live-debug afternoon walked through
  six follow-up tuning commits before we admitted the
  underlying capture architecture and static thresholds were
  the actual problem. This spec captures both fixes so the
  next attempt at fluency doesn't relitigate them. Related:
  shipped/0007-conversational-fluency (the spec whose v1
  surfaced these issues).
