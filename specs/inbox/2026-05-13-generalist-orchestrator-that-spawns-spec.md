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

> **Note — umbrella spec, intentionally remains in `inbox/`.**
> This document holds the *rationale* and the *resolved design
> decisions* for the orchestrator vision. Implementation work
> lives in five child specs (see **Implementation plan** below)
> that each promote/ship independently. The umbrella is not
> meant to be promoted to `active/` or shipped; it stays here
> as the canonical reference for why the children look the way
> they do. Reject it only if the vision itself is abandoned.

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
- [ ] **Workers aren't only `claude --print`.** The orchestrator
      should be able to delegate to any CLI agent the user has
      already installed and authenticated — `gemini-cli`, `oz`
      (formerly warp), `chatgpt`, and friends — picked by the
      task at hand. Jarvis is a force multiplier on the user's
      existing tools, not a re-implementation of token/auth
      handling we'd have to maintain.
- [ ] Spawned workers run in their own process tree and don't
      share the listener's session. Their replies bubble back
      up through the listener, which speaks them via TTS.
- [ ] **Async "spawn and forget" with completion notification.**
      For long-running tasks ("Jarvis, dile a gemini-cli que
      analice este log y avísame cuando termine"), the listener
      backgrounds the worker, returns control immediately, and
      pushes an OS notification (notify-send / native) when the
      worker finishes — separately from any voice/TTS response.
      Unlocks fire-and-forget workflows that the current
      synchronous pipeline can't express.
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
- **PTY support for interactive workers.** `claude --print`
  works fine over plain pipes, but many of the CLI agents
  we'd orchestrate (`oz`, interactive `gemini-cli`, etc.)
  detect a non-TTY stdin/stdout and either buffer output
  weirdly or refuse to run. Spawning interactive workers
  inside a pseudo-terminal (via Rust crates like
  [`portable-pty`](https://crates.io/crates/portable-pty)
  or `nix::pty`) is what lets us capture their streams in
  real time without breaking their expected environment.
  Non-interactive workers (claude print mode, scripted
  shell handlers) skip the PTY and use direct pipes —
  cheaper and simpler. Picked per-worker as a config flag.
- Routing rules could start hand-coded (regex / intent
  table) and evolve toward LLM-based routing if the
  hand-coded set gets unwieldy. Don't reach for the LLM
  until the simple version visibly fails.
- **Explicitly out of scope for the first slice** (Gemini
  proposed these and they're worth a note so we don't
  drift): tmux-style attach/detach for backgrounded
  worker sessions, multi-agent pipelines (piping one
  worker's output as another's input), and a formal
  client-daemon Unix-socket IPC layer. All are interesting
  and tractable, but they solve problems we don't have yet.
  Re-evaluate after the basic listener + spawn flow lands.

## Resolved design decisions

Resolved during refinement session on 2026-05-14. Each links to
the child spec that turns the decision into shippable code.

### 1. The listener is a *cascade*, not a single thing.

Three sequential stages. Stage 2 is optional and pluggable.

```
transcript
   │
   ├─► (stage 1) hand-coded intent matcher  ─► hit  → dispatch directly
   │
   ├─► (stage 2) LlmBackend.classify        ─► intent → dispatch by worker id
   │             (optional; configured under [listener.fallback])
   │
   └─► (stage 3) default worker (current Claude session attach)
```

- Stage 1 is built-in handlers (time, calc, spec management, session
  reset) plus user-installable intent matchers. Sub-millisecond per
  hit, zero RAM tail, zero API cost. Handles the obvious 70-80% of
  voice turns.
- Stage 2 is a `LlmBackend` trait with two implementations at v1:
  `OzCliBackend` (wraps `oz agent run --model <X>` for Warp's open-
  source model lineup) and `OpenAiCompatBackend` (HTTP against any
  OpenAI-compatible endpoint, covering Triton/vLLM/Ollama/Groq/etc).
  The user's GB200 + Triton infra is a first-class target.
- Stage 3 is the existing claude session attach. Unchanged.

Implementation: **hija A** (stages 1 + 3, dispatcher trait, built-in
handlers) and **hija B** (stage 2, LLM backends).

### 2. Memory is hybrid: listener short-term + workers long-term.

Listener guards a per-thread short-term history (~5-10 turns, reuses
the existing `max_turns` cap) plus an `active_workers: HashMap<String,
Option<String>>` mapping worker id → its session id. Stateful workers
(claude, gemini) keep their own long-term context internally and are
resumed by id. Stateless workers (time, calc) receive a fully-resolved
query and return without storing anything.

`session.json` schema is extended in place — the existing turns array
gains `dispatched_to` and `worker_session_id` fields, and a new
`active_workers` top-level field appears.

**Multi-thread is explicitly v1-out:** one conversational thread at
a time. Multiple tasks can run in parallel *within* a thread (via
the task registry below); multiple independent threads are v2.

Implementation: **hija D**.

### 3. Async tasks are tracked, not fire-and-forget.

Each task spawned by the listener has a persistent record in
`~/.cache/jarvis/tasks/<task-id>.json` with status (running |
completed | failed | cancelled | orphaned), captured output path, and
metadata. The user queries the registry via voice or CLI; OS
notifications fire on completion.

Tasks are tied to the conversational thread that spawned them via
`thread_id`, so "¿qué tareas tengo?" defaults to the active thread.
Multi-task within a thread is supported (parallel claude + gemini
both spawned in one conversation); multi-thread is v2.

Split into two shipping phases:

- **Foundation:** registry + CLI commands (`jarvis task list/show/
  cancel/clean`) + completion watcher + notifications.
- **Voice surface:** voice intents ("qué tareas tengo", "cancela la
  última", "muéstrame el resultado de X") layered on the foundation.

Implementation: **hijas E1** and **E2**.

### 4. Workers are declared per-file in `~/.config/jarvis/workers/`.

External workers (claude, gemini-cli, oz, custom) live in
`~/.config/jarvis/workers/*.toml`, one TOML per worker, autodiscovered
at daemon startup. Schema fields: `id`, `description`, `command`
(with `{prompt}` / `{session_id}` / `{cwd}` placeholders),
`initial_command` (for stateful workers on first invocation),
`stateful`, `session_id_capture`, `async_eligible`, `tty`, and
`dispatch_hint` (free-form text consumed by stage 2 LLM dispatcher).

Built-in handlers (time, calc, spec, session-reset) are *not* in
manifests — they're code in `src/handlers/` that self-registers at
startup and is exposed to the dispatcher alongside external workers.

Plugin-style (executable + `--describe` handshake) is left as future
work; the manifest format is sufficient for v1.

Validation at startup: malformed manifests or missing binaries warn-
and-disable the affected worker. The daemon never crashes due to a
bad manifest. `jarvis worker list` shows all workers with status.

Implementation: **hija C**.

## Implementation plan

Six child specs, four phases. Each child is shippable independently
once its dependencies are met.

```
Phase 1 — Foundation (no user-visible behavior change)
  ├─ C  Worker manifests + autodiscovery
  └─ D  Multi-worker memory schema (extend session.json)

Phase 2 — First user value (deterministic intents)
  └─ A  Dispatcher trait + built-in handlers
       deps: C (workers must be declarable), D (memory schema)

Phase 3 — Async delegation
  └─ E1  Task registry foundation + CLI + notifications
        deps: C (workers need async_eligible flag)

Phase 4 — Polish & extensibility (parallelisable)
  ├─ B   LLM dispatcher backends (OzCli, OpenAiCompat)
  │      deps: A (dispatcher trait), C (worker dispatch_hints)
  └─ E2  Voice intents over the task registry
         deps: A (intent matcher), E1 (registry exists)
```

Dependency rules:
- **C must land first.** Everything else references the manifest schema.
- **D can land in parallel with C.** Both are no-behavior-change schema
  work.
- **A blocks on C+D.** Needs workers to be declarable and the session
  schema to support multi-worker.
- **E1 blocks on C only.** Doesn't strictly need A to function as a
  CLI surface, though voice triggers via A make it useful.
- **B and E2 are independent of each other** and both land after A
  and E1 respectively.

Status of children — **all shipped 2026-05-14**:

- ✅ `shipped/0008-orchestrator-c-worker-manifests-and-auto.md` — hija C
- ✅ `shipped/0009-orchestrator-d-multi-worker-memory-schem.md` — hija D
- ✅ `shipped/0010-orchestrator-a-dispatcher-trait-and-buil.md` — hija A
- ✅ `shipped/0011-orchestrator-e1-task-registry-foundation.md` — hija E1
- ✅ `shipped/0012-orchestrator-e2-voice-intents-over-task.md` — hija E2
- ✅ `shipped/0013-orchestrator-b-llm-dispatcher-backends.md` — hija B

## Journal

- 2026-05-14: **umbrella complete — all six children shipped.**
  Final shipping order: C (0008) → D (0009) → A (0010) → E1
  (0011) → E2 (0012) → B (0013). The orchestrator vision the
  user named on 2026-05-13 is now the cascade running in
  master: built-in handlers + optional LLM classifier +
  default-worker fallthrough, with task registry + voice
  surface + multi-worker memory schema underneath. Two
  resolved-design items deliberately deferred to v2 and not
  in any of the children: (1) multi-thread support (one
  thread today; spec D's `active_workers` map is per-session
  but only one session is live at a time), (2) plugin-style
  worker handshake (`--describe`); manifest TOML is
  sufficient for v1. Both are captured in the relevant
  shipped spec journals.
  This umbrella stays in `inbox/` per its own opening note —
  it lives on as the canonical rationale reference, not as
  open work.

- 2026-05-14: hija C shipped (`0008`). The orchestrator's
  declarative foundation is now in master: worker manifests,
  autodiscovery, the `WorkerHandle` trait shared by built-in
  handlers and external manifests, the starter `claude.toml`
  auto-installed on first run, the `jarvis worker list`
  surface, and PTY support for future interactive workers.
  ClaudeAgent reduced to a registry shim with backwards-compat
  fallback. Suite went from 43 → 94 tests. See
  `shipped/0008-...` for slice-by-slice detail.

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

- 2026-05-14: refinement session resolved all four open
  questions. Decision rationale is preserved inline in
  the "Resolved design decisions" section above. The work
  decomposes into six child specs (created same day in
  `inbox/`), grouped into four implementation phases. This
  umbrella stays in `inbox/` indefinitely as the rationale
  reference; child specs each promote/ship on their own
  cadence. Notable scope cuts agreed: multi-thread support
  (v2), tmux-style attach/detach (out), multi-agent pipelines
  (out), plugin-style worker handshake (future), LLM
  auto-summarisation of task output (out of v1).

- 2026-05-13 (later): the user fed the spec into Gemini and
  got back a more expansive proposal. After honest filtering,
  three ideas were merged into the body above: (a) worker pool
  generalised beyond `claude --print` to any installed CLI agent
  (gemini-cli, oz, chatgpt, …), framing Jarvis as a reuser of
  the user's existing tools rather than a re-implementer of
  auth/token handling; (b) PTY-based spawning as a known
  technical requirement for interactive CLIs that detect
  non-TTY stdio and refuse to behave; (c) async spawn-and-
  forget workflow with OS notification on completion, which
  unlocks fire-and-forget tasks the current synchronous
  pipeline can't express. Three further Gemini proposals
  (tmux-style attach/detach, multi-agent pipelines,
  formal Unix-socket client-daemon IPC) were left
  explicitly out of scope in the How section — interesting
  but solving problems we don't have yet. SQLite for state
  and Android-via-Termux were dropped as overengineering /
  premature respectively.
