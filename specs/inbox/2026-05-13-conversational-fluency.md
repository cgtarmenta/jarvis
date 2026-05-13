---
id:
title: Conversational fluency (barge-in, follow-up, streaming TTS)
status: inbox
owner: unassigned
created: 2026-05-13
verifying:
related:
---

# Conversational fluency: barge-in, follow-up listening, streaming TTS

## Why

Right now every turn requires the wake word and waits for the full TTS
utterance to finish before the user can speak again. That makes Jarvis
feel like a transactional command line rather than a conversation: short
clarifications ("¿y en Tokio?") cost the same friction as a fresh task,
and a long-winded answer can't be interrupted when the user already has
the information they need.

Three independent levers improve the feeling of fluency without changing
the agent contract:

1. **Barge-in.** Let the user interrupt the assistant while it is
   speaking. The mic stays hot during TTS playback; detecting voice
   cancels playback and starts STT.
2. **Follow-up mode.** After Jarvis finishes an answer, keep listening
   for a short window (a few seconds) without requiring the wake word
   again. Conversations chain naturally; the wake word only gates the
   start of a session.
3. **Streaming TTS.** Begin speaking as soon as the agent emits its
   first sentence, instead of waiting for the full response. Cuts
   perceived latency on long answers.

Of the three, follow-up mode is the highest leverage for the smallest
implementation cost and should land first. Barge-in is next (it requires
mic-while-speaking, which means echo handling). Streaming TTS depends on
the agent supporting token streaming and the TTS backend supporting
incremental synthesis, so it lands last.

## What

- [ ] Follow-up listening window after each assistant turn, configurable
      in TOML (`session.followup_window_secs`, default ~6s). Within the
      window the wake word is not required; outside, it is.
- [ ] Follow-up window is cancellable: if the user does not speak, the
      session returns to wake-word gating without any audible cue beyond
      what is already played.
- [ ] Barge-in: speaking over the assistant while TTS is playing
      cancels playback within ~150ms and routes audio to STT.
- [ ] Barge-in does not trigger on the assistant's own playback (no
      self-wake loop). Verified by playing a recording of a prior
      assistant utterance and confirming no STT capture starts.
- [ ] Streaming TTS: first audio frame is emitted before the agent
      response is complete, for backends that support it. Falls back to
      buffered TTS when the backend does not.
- [ ] All three behaviors are individually toggleable in config so a
      user on a noisy environment can disable barge-in while keeping
      follow-up.
- [ ] Tests cover: follow-up window start/stop, barge-in cancellation
      latency, self-wake suppression, streaming TTS fallback path.

## How

Affected areas (sketch, not binding):

- Session loop: today, [[project_overview]]'s pipeline returns to
  wake-listening after each turn. Follow-up mode adds a second listening
  state with a timeout.
- Audio I/O: barge-in needs the mic open during TTS playback. The
  simplest path is energy-gated VAD on the mic stream while playback is
  active, with a self-wake suppressor that compares mic energy against
  the playback signal (or just mutes detection while the speaker is the
  same device). Acoustic echo cancellation is the principled fix but is
  out of scope for v1.
- TTS backends: streaming requires backend-side support. The current
  plugin contract returns a finished audio buffer; we'll need to either
  extend it to a streaming variant or keep a buffered fallback and only
  stream for backends that opt in.

Tradeoffs chosen consciously:

- Follow-up window is time-based, not turn-count-based. Simpler, and the
  user already has the wake word as the "definitely listening" gate.
- Self-wake suppression via energy gating, not AEC. Cheaper, works on
  headphones trivially, and degrades gracefully on speakers (some false
  cancellations rather than a feedback loop).
- Streaming TTS is opt-in per backend. Avoids forcing every plugin
  author to implement incremental synthesis.

## Journal

- 2026-05-13: opened. Three levers proposed; follow-up mode is the
  recommended first slice because it eliminates the most user-visible
  friction (re-wake on every short clarification) with the least
  audio-stack complexity.
