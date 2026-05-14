---
id:
title: Orchestrator E1 — Task registry foundation
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
  - inbox/2026-05-13-generalist-orchestrator-that-spawns-spec.md
  - inbox/2026-05-14-orchestrator-c-worker-manifests-and-auto.md
  - inbox/2026-05-14-orchestrator-a-dispatcher-trait-and-buil.md
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

- [ ] Task record stored as one JSON file per task at
      `~/.cache/jarvis/tasks/<task-id>/record.json`. Schema:
      `{ id, thread_id, worker_id, spawn_time, completion_time,
      status, user_intent, command, pid, exit_code, output_path,
      summary }` with status one of `running | completed | failed
      | cancelled | orphaned`. Output and stderr captured into
      sibling files `<task-id>/stdout.txt` and `<task-id>/stderr.txt`.
- [ ] At daemon startup, scan `~/.cache/jarvis/tasks/`, build an
      in-memory `TaskRegistry`. Any task with `status="running"`
      whose `pid` is dead becomes `status="orphaned"` with a
      summary noting the daemon restart.
- [ ] Workers declared with `async_eligible=true` in their hija
      C manifest can be spawned as tasks. A new entry point in
      the pipeline — `pipeline::spawn_async_task(worker_id,
      prompt)` — handles the fork/detach, creates the task
      record, returns immediately. The synchronous path is
      unchanged.
- [ ] A background watcher thread per active task (or a single
      thread polling all PIDs) updates the task record when the
      child exits: sets `completion_time`, `exit_code`,
      `status`, captures any stdout still pending, optionally
      generates a `summary` (v1: just the first 500 chars of
      stdout; LLM-based summarisation is out of scope).
- [ ] OS notification on completion via `notify-rust` crate
      (Linux: D-Bus → notification daemon; macOS: native
      notification centre; falls back silently if neither is
      available). Notification body is the `summary`. Title is
      worker id + status.
- [ ] CLI surface:
      - `jarvis task list [--all]` — active tasks by default;
        `--all` includes completed/failed/cancelled.
      - `jarvis task show <id-or-prefix>` — prints the record
        plus the contents of `stdout.txt`. Accepts unambiguous
        prefixes (so the user doesn't type the full UUID).
      - `jarvis task cancel <id-or-prefix>` — sends SIGTERM to
        the PID, transitions status to `cancelled`.
      - `jarvis task clean [--older-than 7d]` — prunes tasks
        with terminal status older than the duration.
- [ ] Auto-prune: keep at most N (default 50, configurable via
      `[tasks] max_retained` in `config.toml`) tasks with
      terminal status. Active tasks are never auto-pruned. FIFO
      eviction.
- [ ] Trigger phrase: the pipeline detects a sync-vs-async
      decision based on the user's utterance. v1 uses simple
      keyword detection ("avísame cuando", "cuando termines",
      "déjalo en background", "asincrónicamente") plus a config
      flag per worker. If the user phrase matches a trigger and
      the chosen worker has `async_eligible=true`, the task is
      spawned async. Otherwise sync as today.
- [ ] When a task is spawned, the listener speaks a brief
      acknowledgement via TTS ("listo, te aviso cuando
      gemini-cli termine") and returns to listening.
- [ ] Tests cover: full lifecycle (spawn → completion record
      written → notification fired); orphan detection on
      daemon-restart simulation; cancel sends SIGTERM and waits
      for graceful exit (timeout escalates to SIGKILL after 5
      seconds); auto-prune FIFO behaviour; CLI prefix
      resolution (unique prefix succeeds, ambiguous prefix
      errors clearly); trigger-phrase detection.

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

- 2026-05-14: opened. Blocks on hija C (`async_eligible` flag
  in the worker manifest). Independent of hijas A, B, D for
  the CLI surface, but the trigger-phrase detection lives in
  hija A's intent matcher. Recommended to land after A so the
  trigger-phrase code has a home.
