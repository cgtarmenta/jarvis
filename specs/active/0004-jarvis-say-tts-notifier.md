---
id: 0004
title: jarvis say TTS notifier
status: active
owner: tadeo
created: 2026-05-13
shipped:
verifying:
  - cargo run -- say "hello"
  - echo "piped text" | cargo run -- say -
  - cargo test specs::tests::say  # CLI smoke
related:
  - 0003
id: 
shipped: 
---

# jarvis say TTS notifier

## Why

Long-running tasks — compiles, Claude Code on a multi-step refactor,
deploys, data syncs — often finish while you're doing something else. A
desktop notification helps only if you're looking at the screen. A voice
cue gets your attention regardless of where your eyes are.

Jarvis already owns a TTS pipeline (piper / espeak / command). Exposing
it as a one-shot CLI subcommand turns Jarvis into a notification target
for any process: `cargo build && jarvis say "build ok"`,
`claude --print 'summarise' | jarvis say -`, Claude Code Stop hooks,
cron jobs. Cost: ~50 LOC. Value: every script the user already has
gains a voice channel.

A specific motivating quote from the user:

> *"Mañana estoy trabajando en otro repo en CLI, y quiero que jarvis
> continue mientras hago otra cosa..."*

`jarvis say` is the first piece of that picture — the **agent-to-human**
direction. The voice-to-agent direction (attaching to a Claude Code
session running in another repo) is a separate concern and will get its
own spec.

## What

- [ ] `jarvis say <text>` speaks `<text>` via the configured TTS backend
  and exits with status 0 on success.
- [ ] `jarvis say -` reads stdin until EOF and speaks that, so the
  common pipe form works: `echo hi | jarvis say -`.
- [ ] `jarvis say --voice <id>` overrides `[tts].voice` for that call
  only, without editing config — useful for trying voices.
- [ ] Empty / whitespace-only input is a no-op: exit 0, nothing spoken,
  no subprocess spawn.
- [ ] When the TTS subprocess fails, the command exits non-zero and the
  underlying error is printed to stderr.
- [ ] The README has a "Notify on long-running tasks" section with a
  concrete Claude Code `Stop` hook example (`~/.claude/settings.json`)
  that fires `jarvis say "..."`.
- [ ] CHANGELOG entry lands under `## [Unreleased]`.

## How

`tts::build(cfg.tts.clone())` already returns a `Box<dyn Tts>` keyed off
`[tts].backend`. The handler in `cli.rs` only needs to:

1. Resolve the text: if `text == "-"` (or omitted with stdin piped),
   read from stdin until EOF; otherwise join the positional args with
   spaces.
2. If `--voice` is given, clone the `TtsConfig` and overwrite `voice`
   on the clone before building the engine.
3. Call `tts.speak(&text)?` and propagate the error.

The Claude Code Stop-hook example is documentation only — no code
change inside Jarvis is needed to make hooks work, that's a Claude
Code feature we just need to point readers at.

Open question (left for future iteration, not blocking): should
`jarvis say` queue concurrent calls or interrupt the previous one?
For v1, last-writer-wins is acceptable — the user can pipe with `&&`
when they want serialisation.

## Journal

- 2026-05-13: promoted to active.

- 2026-05-13: opened with a rough draft inside the SDD walkthrough.
- 2026-05-13: sharpened — added explicit acceptance bullets for stdin,
  voice override, no-op on empty, exit-code propagation, README + CHANGELOG.
  Verifying steps point to runnable smoke commands. Ready to promote.
