---
id:
title: Orchestrator A — Dispatcher trait and built-in handlers
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
  - inbox/2026-05-13-generalist-orchestrator-that-spawns-spec.md
  - inbox/2026-05-14-orchestrator-c-worker-manifests-and-auto.md
  - inbox/2026-05-14-orchestrator-d-multi-worker-memory-schem.md
---

# Orchestrator A — Dispatcher trait and built-in handlers

Part of the orchestrator vision. Introduces the cascade
dispatcher (stage 1 + stage 3) and the first set of built-in
handlers that bypass Claude entirely for trivial intents. This is
the first child spec that produces a user-visible win.

## Why

The voice loop today routes every utterance to `claude --print
--resume <uuid>`, which means even "¿qué hora es?" pays the full
cold-spawn cost (15+ seconds on a long session, as the user has
already observed). For a large fraction of typical voice usage,
the right worker is a 5ms local function call, not a heavyweight
Claude session.

Implementing the dispatcher cascade — even with only stage 1
(deterministic intents) and stage 3 (default worker) — turns
those trivial queries into instant responses while keeping
everything else unchanged. Stage 2 (LLM dispatcher) is hija B
and is independent.

## What

- [ ] `Dispatcher` trait introduced in a new `src/dispatcher/`
      module. The trait has a single method:
      `dispatch(&self, prompt: &str, session: &Session,
       registry: &WorkerRegistry) -> Result<DispatchDecision>`
      where `DispatchDecision` carries `{ worker_id: String,
      resolved_prompt: String, session_id: Option<String> }`.
- [ ] Stage 1 implementation: a `BuiltinIntentDispatcher` that
      runs an ordered list of `IntentMatcher`s against the
      prompt. The first matcher to return `Some` decides the
      dispatch. Order of matchers is data, not code (loaded
      from a manifest or fixed order — pick the simpler option;
      v1 fixed order is fine).
- [ ] At least five built-in handlers ship with v1, each
      implementing both `IntentMatcher` (the recognition side)
      and `WorkerHandle` (the execution side, from hija C):
      - `time`: matches "¿qué hora es?", "qué hora",
        "dime la hora", optional "en <city>" suffix.
      - `date`: matches "qué día es", "fecha de hoy", "qué
        fecha", optional "del <year>" / relative day.
      - `calc`: matches simple arithmetic
        ("cuánto es <expr>", "calcula <expr>").
      - `spec` (already exists in `src/specs/intent.rs`):
        refactor into the new structure so it lives alongside
        the others. Behavior unchanged.
      - `session-reset` (already exists as
        `pipeline::is_reset_phrase`): refactor into a built-in
        handler. Behavior unchanged.
- [ ] Stage 3 implementation: a `DefaultWorkerDispatcher` that
      always returns the configured default worker (from
      `config.toml`'s `[listener.default_worker]`, defaulting to
      `"claude"`). This is the no-match fallback.
- [ ] Cascade composition: a `CascadeDispatcher` that holds
      `(stage1, stage3)` and tries each in order. Returns the
      first non-None decision. Stage 2 (LLM, from hija B) plugs
      in as the optional middle element of this cascade — v1 of
      this spec ships with the two-stage cascade and a
      well-defined slot for stage 2.
- [ ] Pipeline integration: `pipeline::run_turn` calls the
      dispatcher to get a `DispatchDecision`, then invokes the
      chosen worker via the `WorkerRegistry` (hija C). The
      dispatch decision and resulting session id (if any) are
      recorded into the turn via the hija D schema.
- [ ] Latency budget: end-to-end built-in-handler turn (from
      transcript ready to TTS-ready text) under 100 ms on the
      developer machine. Time/date/calc handlers themselves
      finish in under 10 ms.
- [ ] Tests cover: each built-in matcher recognises its expected
      phrases (positive cases) and rejects close-but-unrelated
      phrases (negative cases); the cascade falls through stage
      1 to stage 3 when no match; the default-worker config
      respected; dispatch decision recorded in the session turn
      with correct `dispatched_to` value.

## How

Implementation notes:

- Reuse `src/specs/intent.rs` as the existing template. It
  already has the pattern: a `recognize` function returning
  `Option<Intent>`, executed by an `execute(intent)`. Generalise
  this into the `IntentMatcher` trait.
- Time and date handlers use `chrono`. For "en Tokio" suffix,
  v1 only supports a hard-coded city → IANA timezone table for
  ~50 major cities. Better lookup is v2.
- Calc handler: use a small expression parser. `meval` or `evalexpr`
  crates exist; pick whichever is leaner. Reject anything that
  isn't a recognisable arithmetic expression (no Python eval).
- The order of matchers in stage 1 matters: spec management
  goes first (it's the most specific keyword-based match),
  then session-reset, then time/date/calc. Misorder = wrong
  worker fires.
- `pipeline::run_turn` becomes thinner: it transcribes, runs
  the dispatcher, invokes the chosen worker via the registry,
  records the turn, speaks the reply. The current Claude-path
  code moves entirely into the default worker (which is a
  registry entry from hija C).
- Latency measurement: add a simple timing log at the
  dispatcher level. Not a benchmark suite — just a
  `tracing::debug!("dispatch decided in {ms}ms")` so we can see
  it in `RUST_LOG=jarvis=debug` runs.

Out of scope:
- LLM-based intent matching (hija B).
- Adding intents beyond the five above. Once the framework
  exists, new built-in handlers are small follow-up PRs.
- Multi-language intent matchers (today everything is Spanish
  with a sprinkle of English). v1 stays Spanish-first.

## Journal

- 2026-05-14: opened. Blocks on hija C (workers must be
  declarable as a precondition) and hija D (memory schema
  must support `dispatched_to` / `worker_session_id`).
  Unblocks hija B (which inserts a stage-2 dispatcher into
  the cascade) and hija E1 (whose task-triggering voice
  phrase is itself a built-in intent matcher).
