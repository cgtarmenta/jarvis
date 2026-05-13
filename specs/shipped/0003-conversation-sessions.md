---
id: 0003
title: Conversation sessions
status: shipped
owner: tadeo
created: 2026-05-13
shipped: 2026-05-13
verifying:
  - cargo test --lib session
  - cargo run -- session show
  - cargo run -- listen   # second invocation continues prior context
related:
  - 0001  # agents trait gained the history parameter
---

# Conversation sessions

## Why

Without continuity, Jarvis is a series of one-shot Q&As. You ask "¿qué
hora es en Tokio?" and the agent answers — but the next wake event the
agent has no memory of the question, so "¿y en Nueva York?" has no
context to attach to. Every interaction is a cold start.

A second, deeper motivation: as the project grows, we want to use
Jarvis itself to drive development through specs (see this directory).
Without persistent state, voice-driven spec creation across multiple
wake events is impossible: each wake would forget the spec under
construction.

The goal is to bridge wake events with a rolling conversation history
that any agent — CLI, HTTP, or shell — can consume.

## What

- [x] A JSON-backed `Session` lives at
  `~/.cache/jarvis/sessions/current.json` with atomic writes (tmp →
  rename) so a crash mid-turn can't leave a half-serialised file.
- [x] The `Agent` trait's `respond` takes `&[Turn]` history. Each
  backend uses it natively: CLI agents (Claude, Warp, shell) embed it
  as labelled `User: / Assistant:` blocks; HTTP agents (OpenAI, Gemini)
  build a `messages` array.
- [x] `[session]` config block with `enabled`, `ttl_seconds` (idle
  cutoff), `max_turns` (truncation cap), and `reset_phrases`.
- [x] Voice reset: if the entire user utterance (case-insensitive,
  accent-stripped) matches any reset phrase, the session is wiped and
  the agent is not called.
- [x] CLI subcommand `jarvis session show | reset | path` for
  inspection and manual cleanup.
- [x] Default `max_turns = 30` keeps the prompt token budget bounded
  without requiring explicit tuning for most users.
- [x] Backward compatible: `[session]` is a new config section with
  defaults, so existing v2 configs pick up sane defaults without a
  schema bump.

## How

Single global "current" session — no multi-session for v1. The user can
always reset; multiple concurrent named sessions would add complexity
without a clear use case at this stage.

Truncation runs both *before* the agent call (so we never send more
than `max_turns`) and *after* writing the new turn pair (so the file
size stays bounded long-term).

Reset phrases are matched against the **entire** normalised utterance,
not as substrings. Otherwise an innocent question like "¿puedes olvidar
la última cosa que dije?" would unintentionally wipe the session.

## Journal

- 2026-05-13: chose single global session over per-agent sessions.
  Rationale: switching agents mid-conversation is rare; when it happens
  the new agent reading the prior agent's reply is usually correct
  behaviour anyway.
- 2026-05-13: rejected long-running agent subprocesses (`claude
  --interactive` kept alive). Output parsing for "is the model done?"
  is brittle; stateless `claude --print` with embedded history is
  reliable and almost as fast thanks to prompt caching.
- 2026-05-13: confined reset phrase matching to whole-utterance match
  after considering substring match. Substring match has too many
  false positives in natural language.
- 2026-05-13: Gemini API uses "model" not "assistant" as the role; the
  Gemini agent translates from our internal `Role::Assistant` on the
  way out.
