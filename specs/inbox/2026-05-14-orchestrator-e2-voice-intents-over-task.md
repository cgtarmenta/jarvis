---
id:
title: Orchestrator E2 — Voice intents over task registry
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
  - inbox/2026-05-13-generalist-orchestrator-that-spawns-spec.md
  - inbox/2026-05-14-orchestrator-e1-task-registry-foundation.md
  - inbox/2026-05-14-orchestrator-a-dispatcher-trait-and-buil.md
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

- [ ] Built-in intent handlers added to the dispatcher (hija A's
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
- [ ] Resolution heuristics for "esa", "la última", etc:
      - "la última" / "esa" → most recent task by spawn_time.
      - "la de gemini" / "lo de claude" → most recent active
        task with matching worker_id.
      - "el análisis del log" → fuzzy match against
        user_intent texts.
      - Ambiguity: if two tasks tie, the listener asks for
        disambiguation ("tienes dos tareas que coinciden,
        ¿la de gemini o la de claude?") instead of guessing.
- [ ] When a task being shown is still running, the response
      acknowledges that ("está corriendo desde hace 3 minutos,
      todavía no termina") instead of fabricating output.
- [ ] When a task has been auto-pruned, the listener says so
      explicitly ("esa tarea ya se purgó del registro,
      conservamos los últimos cincuenta resultados") rather
      than "task not found".
- [ ] All four intents work both during a follow-up window
      (mid-conversation, after the daemon has responded) and
      after the wake word (cold start of a new turn).
- [ ] Tests cover: each intent pattern matches expected
      phrases; resolution heuristics against a populated test
      registry; the ambiguity disambiguation branch; the
      "task purged" and "task still running" response shapes.

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

- 2026-05-14: opened. The smallest of the children — almost
  pure voice-surface layering over E1's foundation. Blocks
  on E1 (registry exists) and A (intent matcher
  infrastructure exists). Recommended to ship after A and
  E1 are both stable.
