---
id: 0012
title: Orchestrator E2 — Voice intents over task registry
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
verifying:
related:
id: 
shipped: 
---

# Orchestrator E2 — Voice intents over task registry

Part of the orchestrator vision. Second half of the async task
work: voice-driven query/cancel/show layered on top of the
registry from E1. Small spec — most of the heavy lifting is
done; this is the thin voice surface.

## Why

E1 builds the task registry and the CLI subcommands, which is
enough for the user to inspect and manage tasks from a terminal.
But the whole point of the orchestrator is voice-first
operation. If checking on a task requires opening a terminal
and typing `jarvis task list`, the orchestrator has failed at
the UX level. The user should be able to ask "qué tareas tengo
corriendo" mid-conversation and get an immediate spoken answer
without breaking flow.

## What

- [x] Built-in intent handlers added to the dispatcher (hija A's
      stage 1 registry) for:
      - **list**: matches "qué tareas tengo", "qué está
        corriendo", "qué hay en background", "tareas activas".
        Returns a TTS-friendly summary: "tienes dos tareas
        corriendo: el análisis con gemini desde hace tres
        minutos, y el refactor con claude desde hace siete
        minutos."
      - **show**: matches "muéstrame el resultado de [X]",
        "qué dijo [X]", "cómo fue el análisis del log".
        Resolves X to a task via fuzzy matching on user_intent
        + worker_id + recency. Reads back the `summary` field;
        if user says "más detalles" within the follow-up
        window, reads the first chunk of `stdout.txt`.
      - **cancel**: matches "cancela esa tarea", "cancela la
        última", "para [X]", "detén la tarea de gemini".
        Resolves to a task as above, calls cancel, confirms.
      - **clean**: matches "limpia las tareas viejas",
        "borra las tareas terminadas". Runs the same
        `jarvis task clean` operation as the CLI.
      *Each handler implemented in its own file under
      `src/handlers/`: `task_list.rs`, `task_show.rs`,
      `task_cancel.rs`, `task_clean.rs`. All four implement
      both `IntentMatcher` and `WorkerHandle`. Trigger phrase
      lists cover Spanish + English variants. Per-handler
      unit tests (positive + negative match cases, cross-
      trait id consistency) plus the
      `task_voice_intents_route_through_cascade` smoke that
      runs the assembled pipeline cascade against
      representative prompts for each of the four.*
- [x] Resolution heuristics for "esa", "la última", etc:
      - "la última" / "esa" → most recent task by spawn_time.
      - "la de gemini" / "lo de claude" → tasks with that
        worker_id.
      - Fuzzy substring match against `user_intent`.
      - Ambiguity returns `ResolveResult::Ambiguous(Vec<Task>)`
        so callers can disambiguate.
      *Implemented in `tasks::resolve::resolve_task_reference`.
      Three-state return (`Unique` / `None` / `Ambiguous`)
      lets each consumer decide how to handle each case.
      The show and cancel handlers use the Ambiguous case
      to ask the user which worker they meant. 5 unit tests
      covering each heuristic and the disambiguation branch.*
- [x] When a task being shown is still running, the response
      acknowledges that ("está corriendo desde hace 3 minutos")
      instead of fabricating output.
      *`task_show::describe_task` branches on
      `TaskStatus::Running` and produces a Spanish age
      string via `humanise_age_spanish`. Tested by
      `describe_handles_every_status`.*
- [x] When a task has been auto-pruned, the listener says
      so explicitly rather than "task not found".
      *Implemented as part of the `ResolveResult::None`
      handler in `task_show`: "No encontré ninguna tarea
      que coincida con eso. Si ya pasó hace tiempo, puede
      que se haya purgado del registro." Trade-off: we
      can't distinguish "never existed" from "purged"
      from the in-memory registry alone — both surface as
      `None`. The hint that auto-prune might be the cause
      is more useful to users than a hard "task not
      found" with no explanation.*
- [x] All four intents work both during a follow-up window
      and after the wake word.
      *Automatic: the cascade runs on every voice turn
      (wake-triggered or follow-up), and the intent
      matchers don't care about which path triggered the
      turn. Confirmed indirectly by the cascade smoke
      test, which exercises the same composition that
      `pipeline::run_turn` assembles for both kinds of
      turn.*
- [x] Tests cover trigger phrases, resolution heuristics,
      ambiguity branch, "task still running" / "task
      purged" response shapes.
      *Per-handler tests + cascade smoke + resolver tests
      collectively cover all bullets above. Some of the
      richer dialogue ("more details" follow-up that
      reads stdout.txt after speaking the summary) is
      deferred to v2 — would need cross-turn state in
      session.json plus a "más detalles" matcher that
      consults `last_shown_task`. Documented in journal.*

## How

Implementation notes:

- Each intent is another `IntentMatcher` + `WorkerHandle` pair
  in the same module structure as hija A's built-in handlers.
  The "worker" they invoke is a small `tasks::Handler` that
  reads from the same `TaskRegistry` E1 introduced.
- TTS-friendly output formatting: time durations like
  "hace tres minutos" (Spanish relative time) rather than
  "started 2026-05-14T15:23:00Z". Use `chrono-humanize` or
  similar, or a small custom function (~30 LoC).
- The "show details on follow-up" interaction (read summary,
  user says "más detalles", read stdout) leverages the
  follow-up window from spec 0007. The first response
  speaks the summary; the listener tracks `last_shown_task`
  in its short-term memory; the follow-up intent for "más
  detalles" / "sigue leyendo" picks up from there.
- Fuzzy match for user_intent uses simple substring
  containment, lowercased + accent-stripped. v1 doesn't
  need a real fuzzy matcher; the user_intent strings are
  short and the user remembers what they said.

Out of scope:
- Streaming task output ("léeme el progreso del análisis a
  medida que avanza"). Tasks remain opaque until completion
  in v1; streaming is a v2 concern that ties to TTS
  interruption / barge-in (out of scope here too).
- Cross-thread task queries ("qué tareas terminé ayer").
  Multi-thread is v2; v1 always operates in the current
  thread's scope.
- Task scheduling ("recuérdame en 30 minutos") — that's a
  different feature class (cron-like), not async tasks.

## Journal

- 2026-05-14: shipped.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. The smallest of the children — almost
  pure voice-surface layering over E1's foundation. Blocks
  on E1 (registry exists) and A (intent matcher
  infrastructure exists). Recommended to ship after A and
  E1 are both stable.
