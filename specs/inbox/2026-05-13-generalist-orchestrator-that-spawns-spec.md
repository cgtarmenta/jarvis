---
id:
title: Generalist orchestrator that spawns specialized workers
status: inbox
owner: unassigned
created: 2026-05-13
shipped:
verifying:
related:
---

# Generalist orchestrator that spawns specialized workers

> **Note — vision-level entry, not a finalised spec.** This was
> captured at the end of a long live-debugging session where the
> user articulated that the model behind Jarvis no longer matches
> what they imagined originally. Treat the "What" bullets below
> as starting points to refine, not as agreed acceptance criteria.

## Why

The current architecture is a thin voice frontend over a single
`claude --print --resume <uuid>` session: every voice turn cold-
spawns Claude Code against the same conversation file, which
means every turn (a) pays the full session-reload latency
documented in `project_voice_turn_latency.md`, and (b) routes
*every* request — quick clarifications, deep coding work,
calendar lookups, all of it — through one monolithic agent
context that wasn't picked for any of them.

The user's mental model is closer to: "I want a generalist
agent that *listens to me*, decides what each request needs,
and spawns the right specialised worker to actually do it." A
quick "¿qué hora es?" should hit a tiny fast agent. A
"refactor this function" should hit Claude with the repo
context loaded. A spec-management voice command should hit
the deterministic spec handler we already wrote. The voice
layer's job is dispatch, not work.

Restating the unsolved tensions this would address:

- **Latency.** Cold-spawning Claude Code per turn is unbearable
  for short turns. A persistent generalist listener with a
  cheap routing decision is fast for the 80% of requests that
  don't actually need the heavy session.
- **Context contamination.** Today every voice turn pollutes
  the same Claude session JSONL. Spawning a separate worker
  for an unrelated task keeps the main session clean.
- **Model fit.** Different tasks want different models /
  toolsets / system prompts. The current single-agent design
  picks one for everything.

## What

*To be refined. Vision-level starting points, not commitments:*

- [ ] The voice path terminates in a "listener" agent that is
      cheap to keep warm. Its job is to decide what to do with
      each transcribed utterance, not to do the work itself.
- [ ] The listener has a small, explicit set of dispatch
      options: handle locally (date, time, simple confirmations),
      delegate to a long-running coding agent (current Claude
      session attach), spawn a one-shot worker (`claude --print`
      with a task-specific system prompt), or run a built-in
      handler (spec management, session reset, etc.).
- [ ] Spawned workers run in their own process tree and don't
      share the listener's session. Their replies bubble back
      up through the listener, which speaks them via TTS.
- [ ] The listener is the only thing that needs to be "always
      on" in voice latency terms. Heavy workers spin up on
      demand and shut down when done — their cold-start cost
      is paid only by the user who asked for that specific
      heavy task.
- [ ] Agent identity, model, prompt, and tool surface are
      data, not hard-coded paths. Adding a new specialised
      worker should be a config change plus the worker
      command, not a Rust diff.

## How

Sketch only — design decisions deferred to active-spec phase:

- The current `agents::build` already returns a trait object;
  the listener is *another* `Agent` implementer whose
  `respond` introspects the prompt and picks a downstream
  agent. We probably grow a `Dispatcher` trait that the
  listener implements.
- "Spawn a worker" maps cleanly onto Claude Code's `--print`
  + `--bare` + per-task `--system-prompt-file`, which the
  existing claude agent module already knows how to do. The
  new piece is the deciding step.
- Routing rules could start hand-coded (regex / intent
  table) and evolve toward LLM-based routing if the
  hand-coded set gets unwieldy. Don't reach for the LLM
  until the simple version visibly fails.

## Journal

- 2026-05-13: opened as a vision note at the end of a
  follow-up debugging session. The user's exact words:
  "necesito un agente generalizado que me escuche [y que sea
  capaz de] spawnear nuevos agentes que hagan los trabajos."
  Conversation was being clipped by the follow-up recorder,
  so this is captured incomplete — to be refined together
  before promotion to active. Related: shipped/0007 (the
  follow-up listening spec whose fragility surfaced this
  rethink), inbox/shared-mic-stream-and-adaptive-voice-thr
  (the orthogonal capture-architecture rework), and
  `project_voice_turn_latency.md` memory (the cold-spawn
  cost this would address).
