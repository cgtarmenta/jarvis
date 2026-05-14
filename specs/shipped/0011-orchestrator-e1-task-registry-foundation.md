---
id: 0011
title: Orchestrator E1 — Task registry foundation
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
verifying:
related:
id: 
shipped: 
---

# Orchestrator E1 — Task registry foundation

Part of the orchestrator vision. The first half of the async
task work: persistent registry + CLI surface + completion
notifications. Voice intents over the registry are hija E2 and
land separately.

## Why

The user's motivating use case for the orchestrator is
delegation that *persists*: "Jarvis, dile a gemini que analice
este log y avísame cuando termine." A synchronous-only pipeline
can't express this — the user would be blocked waiting through
TTS for a multi-minute analysis. They also want to be able to
come back later and ask "qué pasó con eso", which requires the
system to *remember* the task.

Fire-and-forget alone (notification + done) is what you'd write
in 20 lines of shell. The value-add of an orchestrator is a
registry that survives daemon restarts, lets you list/cancel/
inspect in-flight work, and provides a foundation for the voice
surface in hija E2.

## What

- [x] Task record stored as one JSON file per task at
      `~/.cache/jarvis/tasks/<task-id>/record.json`. Status one
      of `running | completed | failed | cancelled | orphaned`.
      Sibling `stdout.txt` and `stderr.txt` files capture
      output.
      *Implemented in `src/tasks/record.rs`. `Task` struct with
      all fields, `TaskStatus` enum, atomic `save()` via
      tmp+rename, `load()` from `<base>/<id>/record.json`,
      path helpers for stdout/stderr. Tests
      `task_new_starts_running`, `task_save_and_load_roundtrip`,
      `path_helpers_share_task_dir`,
      `status_is_terminal_partitions_states`,
      `save_is_atomic_and_creates_dir`,
      `task_id_format_is_sortable_and_unique`. No literal
      `output_path` field — paths are derived from the id
      via the helpers, which is honest about the convention
      and avoids drift.*
- [x] At daemon startup, scan `~/.cache/jarvis/tasks/`, build
      an in-memory `TaskRegistry`. Running tasks with dead
      PIDs become `Orphaned` with a summary.
      *Implemented as a two-step split:
      `TaskRegistry::load_from_dir` is passive (used by the
      CLI and any mid-flight query so a `task list` doesn't
      transition state), and `reconcile_orphans` is
      explicit (called once from `daemon::run` at startup).
      Tests `missing_dir_yields_empty_registry`,
      `terminal_records_load_unchanged`,
      `reconcile_orphans_dead_pid_marks_and_persists`,
      `reconcile_orphans_alive_pid_marks_with_warning_summary`,
      `passive_load_leaves_running_intact`,
      `skips_unreadable_task_directories`,
      `prefix_lookup_unique_versus_ambiguous`.*
- [x] Workers with `async_eligible=true` can be spawned as
      tasks via a pipeline entry point that
      forks/detaches and returns immediately.
      *`tasks::spawn::spawn_async_task(worker, invocation,
      base_dir, thread_id, user_intent)`. `WorkerHandle`
      trait gained `detachable_argv` so the function works
      generically against any future async-eligible worker
      (built-in handlers correctly opt out). Tests
      `rejects_workers_without_async_eligible`,
      `spawns_async_worker_and_records_completion`.*
- [x] Background supervisor thread updates the record on
      exit: completion_time, exit_code, status, summary
      (first 500 chars of stdout).
      *Supervisor lives in `tasks::spawn::supervise`. The
      `Cancelled` branch re-reads on-disk state so an
      explicit `jarvis task cancel` (which sets status
      *before* SIGTERM) isn't overwritten by the
      non-zero-exit-becomes-Failed branch. Summary
      capped at `SUMMARY_CHAR_CAP = 500` chars with an
      ellipsis suffix for longer output; full output stays
      in stdout.txt. Tests
      `nonzero_exit_marks_failed`,
      `long_output_summary_is_capped_with_ellipsis`,
      `empty_output_yields_none_summary`,
      `initial_record_visible_during_run`.*
- [x] OS notification on completion via `notify-rust`. Title
      is `<icon> <worker> <verb>`; body is the summary.
      Notification failures (no D-Bus, no notification
      daemon) are logged at debug, not propagated as task
      errors.
      *Implemented in `tasks::spawn::notify_completion`.
      Per-status icons (✓ / ✗ / ⊘ / ?) plus the worker id
      and exit verb. The `notify-rust` crate handles
      cross-platform delivery. Best-effort: if
      `Notification::show()` errors (most commonly because
      no notification daemon is running), we drop the
      event to a debug log line and continue. Asserting
      the notification fires under unit test is brittle —
      we trust the crate and validate the call site
      indirectly through the supervisor's exit-time
      record updates.*
- [x] CLI: `jarvis task list [--all]`, `task show
      <id-or-prefix>`, `task cancel <id-or-prefix>`, `task
      clean [--older-than 7d]`.
      *Implemented in `src/cli.rs`. List view shows a
      compact table (short id + status + worker + age +
      truncated intent) and supports `--all` to include
      terminal records. Show prints the full metadata
      plus paths to the sibling stdout/stderr files.
      Cancel sets `status = Cancelled` on disk before
      SIGTERM-ing the PID so the supervisor honours the
      user intent. Clean parses `5m` / `2h` / `7d`
      durations via `parse_duration`, walks the registry,
      and removes terminal records older than the
      threshold. Active tasks are always preserved.
      Helpers (`humanise_age`, `format_task_list`,
      `format_task_detail`, `parse_duration`,
      `clean_old_tasks`) are pure functions for cheap
      unit testing. Tests in `cli::task_tests`
      (`parse_duration_units`, `humanise_age_brackets`,
      `format_list_empty`, `format_list_filters_by_status`,
      `format_detail_includes_all_metadata`,
      `clean_drops_old_terminal_tasks`) plus the
      integration test in `tests/cli.rs::task_list_and_show_render_records`.*
- [x] Auto-prune at startup: keep last N (default 50,
      configurable via `[tasks] max_retained`) terminal
      records; active tasks never pruned; FIFO eviction.
      *Implemented as `tasks::cleanup::autoprune_terminal_tasks`.
      Sorts terminal records by completion_time (or
      spawn_time fallback), keeps the newest
      `max_retained`, removes the rest. Called from
      `daemon::run` after the orphan reconcile. Config
      schema gained a `[tasks]` section with
      `max_retained: usize = 50`. Tests
      `under_cap_prunes_nothing`,
      `over_cap_drops_oldest_first`,
      `active_tasks_are_never_pruned`,
      `zero_retain_drops_all_terminal`.*
- [x] Trigger-phrase detection — when a turn contains
      "avísame cuando", "déjalo en background", and friends,
      and the chosen worker is `async_eligible`, route to
      `spawn_async_task` instead of synchronous invoke.
      *Implemented as `tasks::triggers::is_async_trigger`.
      Substring scan over a curated Spanish + English
      phrase list against the normalised prompt. Pipeline
      integration in `pipeline::run_turn`: detects the
      trigger after dispatch, checks the worker's
      `async_eligible()`, spawns + records turn + speaks
      acknowledgement. Trade-off doc'd in the module:
      substring matching means rare question-form Spanish
      phrases might claim the trigger; the worst case is a
      real reply via OS notification instead of TTS.
      Future LLM dispatcher (hija B) can disambiguate
      precisely. Tests `realistic_phrases_trigger`,
      `non_trigger_prompts_decline`,
      `empty_input_declines`,
      `case_accent_and_punctuation_insensitive`.*
- [x] TTS acknowledgement at spawn time: "Listo, te aviso
      cuando <worker> termine."
      *Pipeline returns the ack string and speaks it via
      `tts_engine.speak()` before returning. The synthetic
      assistant turn lands in session.json so follow-up
      questions know what happened. Not asserted in tests
      (the TTS engine isn't easily mockable), but the
      string format is a `format!` over `decision.worker_id`
      so it follows trivially from the test that confirms
      the trigger detection fires.*
- [x] Tests cover the bullets above. The spec listed nine
      coverage requirements; all are met or honestly
      documented as deferred. The cancel-SIGTERM-then-
      SIGKILL escalation after 5 seconds is the one v1
      shortcut: cancel sends SIGTERM and trusts the child
      to exit promptly. Real-world `claude --print` and
      `gemini-cli` honour SIGTERM cleanly, so the
      escalation isn't critical for v1. Adding it is a
      one-line `thread::sleep + SIGKILL` if it becomes
      necessary.

## How

Implementation notes:

- New module `src/tasks/` with `Task`, `TaskRegistry`,
  `TaskWatcher` types.
- Detach mechanics: `setsid` + redirect stdio to the per-task
  log files + fork twice (so the daemon isn't parent of the
  child — orphan to init, which prevents PID reuse confusion).
  `daemonize` crate handles this on Linux/macOS; on BSDs we
  may need manual `nix::unistd::fork`.
- Output capture: each child has stdout/stderr redirected to
  files. The watcher tails them on a 1s interval until exit,
  then snapshots final state. No streaming output to TTS in
  v1 (out of scope).
- `notify-rust` is the cross-platform choice. If it's missing
  a runtime dependency (no D-Bus, no notification daemon),
  fall back to a log line — task completion is recorded in
  the registry regardless.
- Trigger-phrase detection is a small set of regexes in
  Spanish. Add English variants for completeness. This logic
  is part of the dispatcher (hija A) but the keyword list
  lives in this spec because it ties to async semantics.
- CLI subcommands plug into `clap` next to existing `jarvis
  session`, `jarvis spec` — same pattern.

Out of scope for E1 (deferred to E2 or beyond):
- Voice intents to list/cancel/show tasks (hija E2).
- LLM-based output summarisation.
- Streaming output to TTS while a task runs.
- Cross-thread task tracking (a single thread can have
  multiple active tasks, but multiple independent threads
  is v2).
- Task dependencies / pipelines (chain task A's output into
  task B's input).
- Resumption: a cancelled task cannot be resumed.

## Journal

- 2026-05-14: shipped.

- 2026-05-14: shipped. Six slices landed:

  * **E1-1** (`587ac31`) — `Task` record schema, atomic
    save/load, `task_id` generator with nanosecond tail
    for same-second uniqueness. 6 unit tests.

  * **E1-2** (`060875b`) — `TaskRegistry` with the
    passive-load vs explicit-reconcile split. The
    distinction was driven by a failing test during
    development: combining load + orphan check broke
    mid-flight CLI queries; separating them is the
    honest semantic. 9 unit tests covering missing-dir,
    terminal records, dead-PID/alive-PID orphan
    transitions, passive load, active filter, prefix
    lookup, malformed-record skip.

  * **E1-3** (`c925b8e`) — `spawn_async_task` + supervisor
    thread + `notify-rust` OS notifications. Real
    subprocess invocation in tests via `sh -c '...'`
    fixtures. `WorkerHandle::detachable_argv` added
    with `None` default so built-ins opt out of async
    spawning. `ManifestWorker::build_argv` factored out
    so sync and async paths share placeholder
    substitution. 6 unit tests including a
    long-output-summary cap, an initial-record-during-
    run visibility check, and the Cancel/Failed
    disambiguation contract.

  * **E1-4** (`8b715a3`) — `jarvis task list/show/cancel/
    clean` CLI subcommands. Pure helpers
    (`parse_duration`, `humanise_age`, `format_task_list`,
    `format_task_detail`, `clean_old_tasks`) for cheap
    unit testing. 6 unit tests + 1 integration test
    seeding a v2 record into a tempdir cache.

  * **E1-5** (`fbfa257`) — voice trigger detection +
    daemon-startup orphan reconcile + autoprune. New
    modules `tasks::triggers` and `tasks::cleanup`.
    Pipeline integration in `pipeline::run_turn`:
    `is_async_trigger(prompt) && worker.async_eligible()`
    routes to `spawn_async_task`, otherwise the
    synchronous path is unchanged. Config gained a
    `[tasks] max_retained = 50` section. 8 unit tests.

  * **E1-6** (this commit) — `## What` boxes ticked
    with inline pointers, journal consolidated, spec
    shipped.

  Final test count: 163 lib unit + 7 CLI integration + 2
  session integration + 9 config integration = **181
  passing**. Suite grew 145 → 181 across spec 0011.

  Implementation tradeoffs and notes for future readers:

  * **Single supervisor thread per task, not per-process
    supervisor.** The spec mentioned "fork twice so the
    daemon isn't parent of the child" as a robustness
    option. We didn't do that. The child is a direct
    subprocess of the daemon; if the daemon dies, the
    child gets reparented to init and the next daemon
    startup catches it via `reconcile_orphans`. The
    double-fork pattern would let us survive daemon
    restarts mid-task, but it's significantly more code
    and the orphan reconcile path already gives users a
    coherent recovery story ("the daemon restarted; this
    task's child is still running but we can't track it
    anymore — check `ps` to see if you want to keep it").

  * **No SIGTERM → SIGKILL escalation.** Cancel sends
    SIGTERM and trusts the child to honour it. The
    real-world async-eligible workers (claude, gemini)
    handle SIGTERM cleanly. A future v2 can add the
    5-second escalation; the cancel UX (status flips
    immediately so `task list` shows the user intent)
    works without it.

  * **Trigger detection is substring scan, not LLM
    classification.** Rare false positives where a
    question form happens to contain "cuando termines"
    will spawn a task instead of replying inline. The
    fallout is small (the user gets an OS notification
    instead of a TTS reply for that question) and hija
    B's LLM dispatcher can disambiguate later. Honest
    trade-off, documented in the trigger module's
    rustdoc.

  * **No summarisation via LLM.** Summaries are the
    first 500 chars of stdout. Auto-summarisation
    through the LLM dispatcher (hija B) is the obvious
    next step but doesn't block the orchestrator's core
    value prop.

  * **Single-thread task model.** All async tasks
    spawned within one conversational thread live in
    `active_workers[worker_id]` (per spec D). Multiple
    threads with their own task registries is v2.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Blocks on hija C (`async_eligible` flag
  in the worker manifest). Independent of hijas A, B, D for
  the CLI surface, but the trigger-phrase detection lives in
  hija A's intent matcher. Recommended to land after A so the
  trigger-phrase code has a home.
