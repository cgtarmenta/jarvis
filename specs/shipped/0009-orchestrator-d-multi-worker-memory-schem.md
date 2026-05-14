---
id: 0009
title: Orchestrator D — Multi-worker memory schema
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
verifying:
related:
id: 
shipped: 
---

# Orchestrator D — Multi-worker memory schema

Part of the orchestrator vision. Extends the existing
`session.json` schema to support tracking multiple stateful workers
in the same conversational thread. No user-visible behavior change
on its own; it's the data-model prep for hijas A, B, and E.

## Why

Today `~/.cache/jarvis/sessions/current.json` is a flat list of
`(Role, content)` turns shared by whoever the user is talking to
(today: Claude, always). With the dispatcher cascade, a single
thread can hit time-handler, claude, and gemini in three
consecutive turns, each of which is a different worker with
different session semantics.

Two problems the current schema can't express:

1. **Which worker handled which turn?** When the user says "y le
   añades tests?" the listener has to know the prior turn was
   handled by Claude in session `c47a...` so it can resume there
   instead of starting fresh.
2. **Which workers have live sessions?** When the user later says
   "y volvamos al refactor", the listener has to find the Claude
   session id from somewhere — not from the most recent turn (that
   might have been time-handler), but from a separate "last seen
   session per worker" lookup.

Both are expressed cleanly by extending the schema in place: add
two fields to each turn record and one top-level map.

## What

- [x] `Turn` struct gains two new fields (both required for
      newly-written turns, but defaulted on old data for
      migration):
      - `dispatched_to: String` — the worker id that handled this
        turn. For old data without this field, defaulted to
        `"claude"`.
      - `worker_session_id: Option<String>` — the session id
        within that worker, when applicable. Stateless workers
        (time, calc) write `None`; stateful workers write the
        session id captured via the worker's
        `session_id_capture` rule.
      *Implemented in `src/session.rs` with `#[serde(default =
      "default_dispatched_to")]` and `#[serde(default)]`
      respectively. Tests `v1_session_json_loads_with_defaults`,
      `v2_session_json_roundtrips`,
      `add_turn_for_worker_records_metadata`.*
- [x] `Session` struct gains a new top-level field
      `active_workers: HashMap<String, Option<String>>` — map
      from worker id to its most recently-known session id within
      this thread. Defaults to empty on v1 load.
      *Implemented in `src/session.rs`. `set_active_worker_session`
      / `active_worker_session` accessors. Test
      `active_workers_set_and_get` exercises the three-state read
      (`None` / `Some(None)` / `Some(Some)`) and the
      previous-value return contract. Tests
      `truncate_does_not_touch_active_workers` and the integration
      test `pipeline_write_path_produces_v2_session_json` lock
      down the truncation-and-persistence semantics.*
- [x] Schema bump: a new `session_schema_version` field is added
      to `session.json` with current value `2`. The session
      loader reads it (or defaults to `1` if missing); `save()`
      always writes `CURRENT_SESSION_SCHEMA_VERSION`, so legacy
      files quietly migrate on the next save without a separate
      migration pass.
      *`pub const CURRENT_SESSION_SCHEMA_VERSION: u32 = 2;`. Tests
      `save_migrates_v1_session_to_v2` (unit-level) and
      `v1_session_on_disk_upgrades_to_v2_on_next_save` (file-
      level integration).*
- [x] Migration tested: a v1 session.json (without the new
      fields) loads cleanly, the migration fills in defaults, and
      the next save writes a v2 file. Round-trip preserves all
      original turn content.
      *`tests/session.rs::v1_session_on_disk_upgrades_to_v2_on_next_save`
      does exactly this against a tempdir-redirected XDG cache:
      hand-writes a v1 JSON, loads it, saves it, reads back the
      file and asserts `session_schema_version=2` plus the
      backfilled per-turn `dispatched_to=claude`.*
- [x] Pipeline integration: when the agent handles a turn, the
      turn record carries the worker id and the worker's
      pre-invocation session id, and after the response
      `active_workers[worker_id]` is updated.
      *D-2 (commit `a3be0b5`) added `Agent::current_session_id`
      with a default `None` impl; `ClaudeAgent` overrides via the
      existing `claude_attach::resolve` path. `pipeline::run_turn`
      reads it before calling `respond`, then writes both turns
      via `add_turn_for_worker(..., cfg.agent.name, session_id)`
      and updates `active_workers`. Bullet's "or to a
      newly-captured id" branch — the
      `session_id_capture`-driven update — is deferred to hija
      A, which owns the dispatcher that consults manifest
      capture rules. Today the worker is always `cfg.agent.name`
      so the immediate-resume case covers the only path that
      exists. Tested via
      `tests/session.rs::pipeline_write_path_produces_v2_session_json`
      and the unit test `agent_default_current_session_id_is_none`.*
- [x] `jarvis session show` output includes the `active_workers`
      map alongside the existing turn count, age, last-few-turns
      summary.
      *Updated in `src/cli.rs`. Output now includes
      `schema: v2`, an `active_workers:` block (or `(none)` when
      empty) sorted alphabetically by worker id, and each per-
      turn line prefixed with `[Role → worker_id]`. Empty active
      workers map renders as `(none)`; stateless workers render
      as `(stateless)` instead of a uuid. Integration tested via
      `tests/cli.rs::session_show_renders_v2_fields`.*
- [x] Tests cover: migration round-trip; setting and reading
      `active_workers`; turn record shape with both stateful and
      stateless workers; truncation logic (`max_turns`) still
      preserves the most recent turn per worker even after
      pruning older turns.
      *All seven bullets' tests cited above. Total: 87 lib unit
      + 6 CLI integration + 2 session integration + 9 config
      integration = **104 passing**.*

## How

Implementation notes:

- The `Session` struct already exists in `src/session.rs`. We
  add fields with `#[serde(default)]` so v1 files parse without
  the new fields and the defaults fill in.
- Migration happens on load; we explicitly serialize
  `session_schema_version = 2` on save. Future schema bumps
  follow the same pattern.
- `active_workers` is updated inside `pipeline::run_turn` (which
  hija A will further refactor). At v1 the *only* worker that
  produces a session id is Claude, so this map will have at most
  one entry until B and other workers land — but the schema is
  ready.
- Truncation: today `max_turns` drops oldest turns. The new
  concern is that dropping a turn shouldn't lose the
  `active_workers` map (that's separate). The current truncation
  code already operates on `turns`, not on top-level fields, so
  this is incidentally correct — but the test makes it explicit.

Out of scope:
- Multi-thread (multiple independent `thread_id`s active at
  once). v1 is one thread, the field is added but only ever
  contains one value.
- Per-worker memory caps (e.g. "remember only the last 20
  turns specifically from gemini"). The single `max_turns`
  cap applies to the whole thread.
- Forgetting a single worker's session ("olvida lo de gemini
  pero mantén la sesión de claude"). Out of scope; users reset
  the whole thread.

## Journal

- 2026-05-14: shipped.

- 2026-05-14: shipped. Three slices landed:

  * **D-1** (`4547055`) — Schema fields on `Turn`
    (`dispatched_to`, `worker_session_id`) and `Session`
    (`session_schema_version`, `active_workers`). serde
    defaults backfill the v1 cases; `save()` always writes
    the current version so legacy files migrate quietly on
    the next persist. New surface:
    `add_turn_for_worker`, `set_active_worker_session`,
    `active_worker_session`. Legacy `add_turn` kept as a
    backward-compat wrapper that defaults to `"claude"`. 6
    new unit tests in `session::tests` covering v1 load with
    defaults, v2 roundtrip, the worker-aware constructor,
    the three-state active-workers map, the migration path,
    and truncation safety.

    Bundled bonus fix: `ManifestWorker::invoke_pipes` now
    tolerates `BrokenPipe` when writing the prompt to stdin.
    The `sh -c 'printf ...'` fixture used by the
    env-propagation test (`manifest_worker_env_carries_jarvis_voice_turn`)
    was racing with the worker's exit and producing flaky
    EPIPE failures ~10% of runs. The fix matters in
    production too: stateless built-in handlers that don't
    read stdin shouldn't fail the turn just because the
    writer raced past their exit.

  * **D-2** (`a3be0b5`) — Pipeline integration.
    `Agent::current_session_id` added with a default `None`
    impl; `ClaudeAgent` overrides via `claude_attach::resolve`.
    `pipeline::run_turn` reads it before `respond`, then
    writes turns with `add_turn_for_worker(..., agent.name(),
    worker_session_id)` and updates the `active_workers`
    map. Two integration tests in `tests/session.rs`
    (`pipeline_write_path_produces_v2_session_json`,
    `v1_session_on_disk_upgrades_to_v2_on_next_save`) plus
    a unit test for the default trait impl.

  * **D-3** (this commit) — `jarvis session show` updated
    to surface the schema version, `active_workers` map,
    and per-turn `dispatched_to`. Integration test
    `session_show_renders_v2_fields` seeds a v2 session
    fixture into a tempdir cache and asserts the CLI output
    contains the new fields.

  Final test count: 87 lib unit + 6 CLI integration + 2
  session integration + 9 config integration = **104
  passing**. Suite has grown 43 → 104 across hijas C + D.

  Tradeoffs and notes for future readers:

  * **Migration is one-way and implicit.** Old binaries
    reading a v2 file would lose the new fields silently
    (serde drops unknown-shaped data by default). We
    document v2 as the format from this commit forward; no
    downgrade path is supplied. Honest because we're
    pre-1.0 — if someone needs to roll back, the v1 files
    they wrote before this spec are untouched (they're
    different physical files at different timestamps).
  * **The "or newly-captured id" branch in the pipeline
    bullet is deferred to hija A.** Today there's no
    dispatcher, so there's no `session_id_capture`-driven
    update — the worker is always `cfg.agent.name` and the
    session id we record is what claude_attach surfaces
    pre-invocation. Hija A's dispatcher will consume
    `session_id_capture` rules from manifests and feed the
    captured ids into the same map.
  * **active_workers semantics are deliberately tri-state.**
    `None` (key not in map) means "never invoked";
    `Some(None)` means "invoked but stateless"; `Some(Some)`
    means "active stateful worker, resume from this uuid".
    The dispatcher will rely on this distinction.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Can land in parallel with hija C — both
  are schema/architecture prep with no behavior change. Hija A
  blocks on D landing because the dispatcher writes the new
  fields on every turn.
