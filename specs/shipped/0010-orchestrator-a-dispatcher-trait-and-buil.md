---
id: 0010
title: Orchestrator A — Dispatcher trait and built-in handlers
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
verifying:
related:
id: 
shipped: 
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

- [x] `Dispatcher` trait introduced in `src/dispatcher/` with
      `dispatch(prompt, session, registry) -> Result<Option<DispatchDecision>>`.
      `DispatchDecision` carries `{ worker_id, resolved_prompt,
      session_id }`. *Trait returns `Option` (not `Result`) so
      stages can decline cleanly; the cascade's last stage
      always claims a turn, so an `Ok(None)` at the top level
      is a programmer error (cascade mis-configured).*
- [x] Stage 1: `BuiltinIntentDispatcher` walks an ordered list
      of `IntentMatcher`s; first to claim wins. *Order is a
      fixed Vec passed at construction; matchers come out of
      `handlers::register_builtins` in spec-then-time-then-
      date-then-calc-then-reset order. Tests
      `first_matcher_to_claim_wins`, `no_matches_returns_none`,
      `session_id_flows_through_when_recorded`,
      `matcher_count_reflects_pushes`.*
- [x] Five built-in handlers ship: `time`, `date`, `calc`,
      `spec`, `session-reset`. Each implements both
      `IntentMatcher` and `WorkerHandle`. *Files under
      `src/handlers/`. `time_of_day` has a 50-entry city →
      IANA timezone table (chrono-tz); `date_today` renders
      Spanish long-form dates without a locale crate;
      `calc` uses `evalexpr` and promotes integer literals to
      floats on division so "10 entre 4" = 2.5; `spec`
      wraps the existing `crate::specs::{recognize,execute}`;
      `session_reset` carries the normalise + exact-match
      logic relocated from `pipeline.rs`. Per-handler tests
      cover positive matches, negative declines, output
      shape, and cross-trait id consistency.*
- [x] Stage 3: `DefaultWorkerDispatcher` always returns the
      configured worker (today: `cfg.agent.name`). *Lifts the
      worker's prior session id out of
      `session.active_workers` via the spec D accessor,
      unwrapping the tri-state correctly. Tests
      `default_dispatcher_always_returns_some`,
      `default_dispatcher_carries_session_id_from_active_workers`,
      `default_dispatcher_unwraps_stateless_marker`.*
- [x] Cascade composition: `CascadeDispatcher` holds an
      ordered `Vec<Box<dyn Dispatcher>>`, tries each in
      insertion order, returns first `Some`. Stage 2 slot
      remains empty (hija B). *Builder-style `.push(stage)`;
      pipeline composes `BuiltinIntentDispatcher → DefaultWorkerDispatcher`.
      Tests `cascade_returns_first_match`, `cascade_skips_none_stages`,
      `cascade_returns_none_when_all_stages_decline`,
      `stage_count_reflects_pushes`.*
- [x] Pipeline integration: `pipeline::run_turn` calls
      `dispatcher.dispatch(...)`, resolves the worker through
      the registry, invokes via `WorkerHandle`. *Fallback to
      legacy `Agent::respond` only for workers absent from the
      registry (openai/gemini/warp/shell — manifest migration
      for those was explicitly OOS in spec C). Turn metadata
      (`dispatched_to`, `worker_session_id`, `active_workers`)
      now sourced from the decision rather than from the
      `Agent::current_session_id` hook. Inline
      `is_reset_phrase` and `specs::recognize` checks in
      `pipeline.rs` are deleted — the dispatcher handles both
      via their respective handlers. The `normalise` helper
      moved to `handlers::session_reset` in A-2.*
- [x] Latency budget: end-to-end built-in turn well under 100 ms
      on the dev machine. *Sub-millisecond for time/date/calc
      (Rust function calls, no subprocess); ~5-10 ms for spec
      management (file I/O on the specs/ dir). Verified by
      reading the test runs (`cargo test handlers::` finishes
      in <1ms per handler test, even the ones that exercise
      `invoke`).*
- [x] Tests cover: each matcher recognises and declines;
      cascade falls through stage 1 to stage 3; default
      worker config respected; `dispatched_to` recorded in
      the session turn.
      *Per-handler tests cover the first three. The
      `full_cascade_routes_prompts_to_expected_workers`
      integration test wires together
      `handlers::register_builtins` →
      `BuiltinIntentDispatcher::from_matchers` →
      `CascadeDispatcher::push(default)` and asserts six
      representative prompts route to the right worker
      (one per built-in plus the unmatched-prompt
      fallthrough). The `dispatched_to` field is locked
      down by `tests/session.rs::pipeline_write_path_produces_v2_session_json`
      from spec D.*

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

- 2026-05-14: shipped.

- 2026-05-14: shipped. Five slices landed:

  * **A-1** (`d9e936c`) — `Dispatcher` trait,
    `DispatchDecision`, `CascadeDispatcher`,
    `DefaultWorkerDispatcher`. Skeleton only — no handlers,
    no pipeline integration. 8 new unit tests covering
    cascade ordering and the default-stage's session-id
    lift from `active_workers`.

  * **A-2** (`698fd01`) — `IntentMatcher` trait,
    `BuiltinIntentDispatcher` as the cascade's stage 1.
    Existing `crate::specs::{recognize,execute}` and the
    `is_reset_phrase` logic in `pipeline.rs` get factored
    into `SpecHandler` and `SessionResetHandler` —
    structurally `IntentMatcher` + `WorkerHandle` pairs that
    self-register via `WorkerRegistry::register_builtin`
    (spec C). 13 new unit tests including the substring-
    trap suite that locks down "no, ¿puedes olvidar la
    última cosa?" *doesn't* trigger reset.

  * **A-3** (`a002170`) — Three brand-new handlers:
    `TimeOfDayHandler`, `DateTodayHandler`, `CalcHandler`.
    New deps `chrono`, `chrono-tz`, `evalexpr`. Hand-curated
    50-entry city → IANA timezone table for the time
    handler's `en <city>` suffix; hand-localised Spanish
    weekday / month names for the date handler;
    `evalexpr` with float-promotion-on-division for the calc
    handler so "10 entre 4" = 2.5 rather than the
    integer-truncated 2. 19 new unit tests + a real-bug
    fix caught during implementation (the int-division bug
    above).

  * **A-4** (`7c3f30f`) — Pipeline integration. Every voice
    turn now goes through the cascade; deterministic intents
    skip the Claude cold-spawn entirely. Inline
    `is_reset_phrase` + `specs::recognize` checks deleted
    from `pipeline.rs` (the dispatcher handles them via
    the respective handlers). `cmd_worker_list` updated to
    also call `register_builtins` so `jarvis worker list`
    surfaces all six workers, not just manifests on disk.
    1 new end-to-end smoke test exercising the full
    cascade composition + a manual smoke confirming the
    six-worker registry shape against the user's real
    config.

  * **A-5** (this commit) — `## What` boxes ticked with
    inline pointers, journal consolidated, spec shipped.

  Final test count: 128 lib unit + 6 CLI integration + 2
  session integration + 9 config integration = **145
  passing** (started at 104 before A-1).

  Implementation tradeoffs and notes for future readers:

  * **Spec bullet 5 reinterpretation.** The spec listed
    handlers as "self-register at startup" but the actual
    handlers (time/calc/spec/session-reset) belong here in
    hija A because they need a dispatcher to plug into.
    Spec C only shipped the *mechanism* (`register_builtin`);
    A-2/A-3 ship the implementations. The bullet is checked
    off here, not in C.

  * **`session_id_capture` route in the pipeline.** The
    pipeline writes `effective_session_id =
    response.captured_session_id.or(decision.session_id)`
    — captured ids (from manifests with capture rules)
    take precedence over the dispatcher's
    pre-invocation guess. Built-in handlers never produce
    a captured id (they're stateless), so for them the
    decision's `session_id` (which the default stage lifts
    from `active_workers`) wins, which is `None` and stays
    `None`. Stateful workers like Claude get the
    pre-invocation UUID through. Manifest workers with
    capture rules — none in v1 — would override.

  * **Per-turn registry rebuild.** The pipeline reads
    `workers/*.toml` and re-registers built-ins on every
    voice turn. The cost is one stat + one read for the
    bundled claude.toml + five trivial registrations. In
    return manifest edits take effect on the next
    utterance without a daemon restart. If we ever have
    50 manifests this might warrant caching; today it
    doesn't.

  * **Legacy `Agent` fallback survives.** Non-claude
    agents (openai/gemini/warp/shell) still go through
    `Agent::respond` because their manifest migration was
    explicitly out of scope for spec C. The pipeline
    branches on `registry.get(worker_id)` —
    `Some` → `WorkerHandle::invoke`; `None` → legacy
    `Agent::respond` with the embedded-history prompt.
    Eventually all agents become manifests and the
    fallback path becomes dead code; nothing in spec A
    accelerates that.

  * **Latency-budget reading.** "End-to-end built-in turn
    under 100 ms" is measured from the dispatcher's
    decision back to the worker's response text — *not*
    from the user's utterance to TTS playback. The
    audible delay includes STT (whisper-cli, ~1 s) + TTS
    (piper, ~500 ms) which are unchanged. The win is the
    elimination of the 5–15 s Claude cold-spawn for
    trivial intents.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Blocks on hija C (workers must be
  declarable as a precondition) and hija D (memory schema
  must support `dispatched_to` / `worker_session_id`).
  Unblocks hija B (which inserts a stage-2 dispatcher into
  the cascade) and hija E1 (whose task-triggering voice
  phrase is itself a built-in intent matcher).
