---
id: 0002
title: Pluggable wake-word backends
status: shipped
owner: tadeo
created: 2026-05-13
shipped: 2026-05-13
verifying:
  - cargo run -- test-wake --seconds 5 --phrases jarvis
  - cargo test --lib wake
related:
  - 0001  # same pluggability pattern as agents
---

# Pluggable wake-word backends

## Why

Wake-word detection is the part of Jarvis with the widest range of
acceptable implementations: a 5MB CPU-only string-match against
whisper.cpp transcripts is fine for development boxes, a 30MB ONNX KWS
runtime is the right answer on a desktop, and a custom-trained
rustpotter model fits some users' privacy concerns better than either.

Locking in a single backend would mean every user pays the same
resource cost regardless of their needs and would block contributions
from people who care about a specific backend. Make wake-word a
config-string choice the same way agents are.

## What

- [x] A `WakeBackend` trait in `src/wake/mod.rs` with a `run(on_wake,
  should_stop)` method that owns the audio loop.
- [x] `[wake] backend = "..."` config selects the implementation.
- [x] Implemented: `none` (hotkey-only, the default) and `whisper`
  (reuses `whisper-cli` to transcribe rolling windows and string-match
  against `[wake].phrases`).
- [x] Wire-stub for roadmap backends (`sherpa`, `openwakeword`,
  `rustpotter`) — the wizard offers them but `wake::build` returns a
  clear error so the user knows it's not implemented yet.
- [x] Whisper backend supports custom phrases via config — no model
  training required to add `"mutombo"` or any other wake word.
- [x] `jarvis test-wake` exercises the configured backend for N seconds
  with verbose logging — for tuning thresholds and phrases without
  running the full daemon.
- [x] The whisper backend uses pre-roll buffer (300ms) and hysteresis
  (sustain at 0.5× trigger) to avoid clipping consonant onsets like
  the "m" in "mutombo".
- [x] `[wake].stt_model_override` allows pairing a large model for the
  main listen flow with a tiny/base model for the wake loop.

## How

The trait surface is the smallest possible API:

```rust
pub trait WakeBackend {
    fn name(&self) -> &'static str;
    fn run(
        &self,
        on_wake: &mut dyn FnMut(),
        should_stop: &dyn Fn() -> bool,
    ) -> Result<()>;
}
```

Each backend owns its audio loop; the daemon just plumbs the wake
callback into `pipeline::run_once`. This means each backend can pick its
own audio capture strategy (raw stream for whisper, ONNX-friendly
windowing for sherpa, etc.) without coordinating with the orchestrator.

Whisper-as-wake-word is the only fully-shipped backend so far. The
classic problem with energy-VAD on speech is leading-edge clipping: the
first chunk where RMS crosses the threshold is already mid-syllable, so
the "Mu-" of "Mutombo" gets lost and whisper transcribes "Combo". The
pre-roll buffer rescues that audio. The hysteresis (sustain = 0.5 *
trigger) prevents mid-word silences (between syllables) from
prematurely ending the utterance.

## Journal

- 2026-05-13: chose to ship `whisper` first because it reuses an
  existing dependency (whisper.cpp is required for the main STT path
  anyway). Custom-phrase support comes free.
- 2026-05-13: rejected requiring the user to train their wake word from
  voice samples (the rustpotter model). Too much friction for a v1.
- 2026-05-13: added pre-roll + hysteresis after the first end-to-end
  test failed to capture "mutombo" cleanly. Initial implementation lost
  the leading 100ms; bumping the threshold helped, but pre-roll is the
  proper fix.
- 2026-05-13: added `stt_model_override` after observing that running
  `large-v3` on every wake detection was wasteful — the wake loop
  doesn't need state-of-the-art accuracy, just confident phrase match.
