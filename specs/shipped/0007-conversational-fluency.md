---
id: 0007
title: Conversational fluency — follow-up listening
status: shipped
owner: unassigned
created: 2026-05-13
shipped: 2026-05-13
verifying:
related:
id: 
shipped: 
---

# Conversational fluency — follow-up listening

## Why

Right now every turn requires the wake word. That makes Jarvis feel
like a transactional command line rather than a conversation: short
clarifications ("¿y en Tokio?") cost the same friction as a fresh
task. Re-saying the wake word for each clarification is the most
visible source of unnatural pacing in the current voice loop —
addressing just this one issue should make the assistant feel
materially more like a conversational partner.

This spec focuses **only** on follow-up listening. Two other
fluency levers (barge-in over playback, streaming TTS) are real
but architecturally larger (echo handling, TTS contract change)
and will get their own specs. Scoping tight keeps this one
shippable in one slice.

## What

- [x] Add `session.followup_window_secs` to the TOML config
      (default 6.0; set to 0 to disable). Round-trips through
      `config::load` and is exposed in `config/config.example.toml`.
- [x] After each successful agent turn in the wake-word daemon,
      open a follow-up listening window of that length. Speech
      detected within the window is consumed as the next user
      turn without requiring the wake word. Silence through the
      whole window returns the daemon to wake-word gating.
- [x] Follow-up turns reuse the existing turn orchestration
      (STT → agent → TTS → session persistence) so there is
      exactly one code path responsible for a turn — no drift
      between wake-triggered and follow-up turns. Achieved via
      `pipeline::run_turn(cfg, opts)`, with `run_once` becoming
      a thin wrapper around `run_turn(cfg, TurnOptions::primary())`.
- [x] Follow-up turns skip the "listening cue" beep (the cue is
      a wake-acknowledgement; once we are mid-conversation it
      adds friction instead of removing it). Plumbed via
      `TurnOptions::play_cue`.
- [x] Empty follow-up captures (no speech) end the follow-up
      chain silently — `run_turn` returns `Ok(None)` and the
      daemon's loop falls through to the wake backend without
      any audible "I heard nothing" prompt.
- [x] `jarvis listen` (one-shot, hotkey path) is unchanged.
      The follow-up loop lives only in `daemon::run_followup_chain`.
- [x] Unit + integration tests cover the testable surface:
      config default (`followup_window_default_is_six_seconds`),
      TOML round-trip at zero (`followup_window_zero_is_preserved`),
      TOML round-trip at custom value
      (`followup_window_custom_value_round_trips`), and the
      `TurnOptions::primary` vs `TurnOptions::default` contract
      that the daemon's follow-up branch depends on. Daemon-loop
      iteration count (record → STT → loop) is not unit-tested
      because the audio pipeline has no cheap mock; verification
      is by running `jarvis daemon` and speaking a follow-up
      utterance — a known testing tradeoff documented in the
      journal.

## How

Affected areas:

- `src/config.rs` — extend `SessionConfig` with the new field;
  default 6.0 so the feature is on out of the box.
- `src/pipeline.rs` — introduce `TurnOptions { play_cue,
  record_override }` and split `run_once` into a thin wrapper
  around `run_turn(cfg, opts)`. Follow-up calls pass
  `play_cue=false` and `record_override = Some(RecordConfig {
  max_seconds: followup_window_secs, ..cfg.record.clone() })`.
- `src/daemon.rs` — after a successful `run_once`, loop on
  `run_turn(cfg, follow_up_opts)` until it returns
  `Ok(None)` (no speech) or an error. Then fall through to the
  wake backend's loop as before.

Tradeoffs chosen consciously:

- Follow-up window is time-based, not turn-count-based. Simpler,
  and the user already has the wake word as the "definitely
  listening" gate.
- We rely on the existing recorder's `silenceremove` filter to
  end the recording early when the user stops speaking. If the
  user never speaks, the recorder runs the full window and the
  STT engine returns an empty transcript — a known but
  acceptable latency cost (the user is silent anyway).
- We do not implement leading-silence skipping in v1. The
  recorder writes the leading silence into the WAV; STT
  tolerates it. If this becomes a perceived-latency issue we
  can add `start_periods=1` to the ffmpeg filter chain, but
  doing so changes the wake-triggered recorder too and that
  needs a separate think.

## Journal

- 2026-05-13: post-ship bug fix (v1.1). The user reported during
  live voice testing that follow-up turns cut him off
  mid-sentence at the six-second mark, and that even the
  fifteen-second wake-triggered turns sometimes ran to the
  hard cap without exiting on trailing silence. Root causes
  identified and fixed in one slice:

  1. The v1 follow-up loop set `record.max_seconds =
     followup_window_secs`, conflating "how long to wait for
     speech to start" with "how long to let the user speak."
     Replaced with a real onset-gated recorder:
     `recorder::record_with_onset(cfg, onset_secs)` opens a
     raw-PCM mic, runs RMS-based VAD on 100 ms chunks, waits
     up to `onset_secs` for the user to start, then captures
     the utterance bounded by the *recorder's* normal
     `max_seconds` with the leading edge preserved. Wired
     through `TurnOptions::wait_for_onset_secs` so it stays a
     per-turn opt-in. New helper `TurnOptions::followup(secs)`
     bakes the canonical shape so the daemon and any future
     follow-up call site share one definition.

  2. The trailing-silence threshold (`-40 dBFS`) was hard-coded
     and too strict for normal microphones — ambient noise
     never dipped that low so ffmpeg's `silenceremove` never
     fired and the recorder ran to `max_seconds`. Made it
     configurable as `record.silence_threshold_db` (default
     `-30.0`, documented in `config.example.toml` with tuning
     guidance) and threaded through `build_ffmpeg`.

  Two new unit tests in `recorder::tests` (`rms_to_dbfs_calibration`,
  `silence_threshold_default_is_user_friendly`) lock in the
  dBFS math and the default-threshold contract so silent
  regressions can't ship.

- 2026-05-13: shipped.

- 2026-05-13: implemented. Three changes landed in one slice:
  `SessionConfig::followup_window_secs` (default 6.0),
  `pipeline::TurnOptions` + `run_turn` (with `run_once` now a
  thin wrapper), and `daemon::run_followup_chain` (loops
  `run_turn` with `play_cue=false` and a shortened
  `RecordConfig::max_seconds` until either silence or an
  error).

  Testing tradeoff documented for posterity: the daemon's
  follow-up-loop iteration count is not unit-tested because no
  cheap mock of the STT/recorder/agent stack exists in the
  codebase, and building one just for this would be more
  surface than the feature itself. The cheaply-testable pieces
  (config plumbing, TurnOptions contract) are covered;
  behavioral verification is by running `jarvis daemon` and
  speaking a follow-up. Acceptable for v1 — if the loop later
  develops bugs we'll know because real voice turns will break,
  and at that point investing in mocks is justified.

- 2026-05-13: promoted to active. Scoped down from the original
  three-lever proposal to follow-up only; the other two levers
  (barge-in, streaming TTS) carry their own architectural
  complexity (echo handling, TTS contract change) and will be
  separate specs filed from the inbox.

- 2026-05-13: opened. Three levers proposed; follow-up mode is
  the recommended first slice because it eliminates the most
  user-visible friction (re-wake on every short clarification)
  with the least audio-stack complexity.
