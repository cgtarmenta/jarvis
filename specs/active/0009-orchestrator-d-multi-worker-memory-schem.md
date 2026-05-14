---
id: 0009
title: Orchestrator D — Multi-worker memory schema
status: active
owner: unassigned
created: 2026-05-14
shipped:
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

- [ ] `Turn` struct gains two new fields (both required for
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
- [ ] `Session` struct gains a new top-level field:
      - `active_workers: HashMap<String, Option<String>>` —
        map from worker id to its most recently-known session id
        within this thread. On old data without this field,
        populated from the existing legacy `attached_session_id`
        / similar fields if present, else empty.
- [ ] Schema bump: `config_version` is *not* affected (config
      schema is unchanged); a new `session_schema_version` field
      is added to `session.json` with current value `2`. The
      session loader reads `session_schema_version` and migrates
      v1 sessions to v2 on the next save.
- [ ] Migration tested: a v1 session.json (without the new
      fields) loads cleanly, the migration fills in defaults, and
      the next save writes a v2 file. Round-trip preserves all
      original turn content.
- [ ] Pipeline integration: when the dispatcher routes a turn to
      worker X with session id Y, the turn record records both,
      and after the response `active_workers[X]` is updated to Y
      (or to a newly-captured id if the worker emitted one via
      its `session_id_capture` rule).
- [ ] `jarvis session show` output includes the `active_workers`
      map alongside the existing turn count, age, last-few-turns
      summary.
- [ ] Tests cover: migration round-trip; setting and reading
      `active_workers`; turn record shape with both stateful and
      stateless workers; truncation logic (`max_turns`) still
      preserves the most recent turn per worker even after
      pruning older turns.

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

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Can land in parallel with hija C — both
  are schema/architecture prep with no behavior change. Hija A
  blocks on D landing because the dispatcher writes the new
  fields on every turn.
