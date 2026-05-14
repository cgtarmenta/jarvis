---
id:
title: Daemon context bootstrap — what is the 'current project' when autostarted?
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
  - shipped/0008-orchestrator-c-worker-manifests-and-auto.md
  - shipped/0009-orchestrator-d-multi-worker-memory-schem.md
---
# Daemon context bootstrap — what is the 'current project' when autostarted?

> **Vision-level entry, not a finalised spec.** Captures a real
> observation the user made; the right answer needs discussion before
> code.

## Why

Today `jarvis daemon` inherits its working directory from wherever the
user launched it. The Claude agent shim uses that cwd to drive
`claude_attach::resolve` — which looks under
`~/.claude/projects/<encoded-cwd>/` for prior sessions. So if the user
launches the daemon from `~/github/jarvis`, every voice turn lands in
the Jarvis project's Claude context. The user noticed:

> Cuando corro `jarvis daemon` parece que coge como "root" el PWD
> desde donde lance `jarvis daemon`, porque si le hago preguntas de
> jarvis o así me contesta.

That's by design today, but it doesn't generalise:

- **Autostart on boot** (the obvious next-step UX) launches the daemon
  with cwd = `/` or `$HOME`, neither of which has useful Claude
  session history.
- **Multi-project users** want different conversational contexts at
  different times. The current "PWD wins" model forces them to
  restart the daemon to switch.
- **Project-less queries** ("qué hora es", "abre Firefox") don't care
  about cwd at all — current model wastes a bit of context resolution
  on them.

## What

*To be refined. Possible directions to weigh:*

- [ ] **Option A — Pinned project in config.** A new field
      `[agent].pinned_cwd = "/home/user/main-project"` (or similar)
      overrides PWD at daemon start. User sets it once during
      `jarvis setup` and the autostart story becomes "same context
      every time."
- [ ] **Option B — No project context by default.** Daemon launches
      "stateless" w.r.t. Claude sessions; voice commands like
      "trabajemos en jarvis" or "switch to project X" change the
      active cwd at runtime. Spec D's `active_workers` map naturally
      tracks this per thread.
- [ ] **Option C — First-run interactive bootstrap.** Same as A but
      elicited via `jarvis setup` instead of requiring config-file
      editing. Saves the chosen cwd to config.
- [ ] **Option D — Multi-thread (already in spec D's roadmap).** Each
      conversational thread has its own project context. Voice
      "abre un nuevo thread para X" creates one. Generalises to the
      multi-project case without forcing a single global pin.

## How

This is a design discussion. No implementation sketch yet; the right
direction depends on whether multi-thread is imminent (favours D) or
far off (favours A + C as a shorter-term shim).

Things to consider before picking:

- **Autostart UX.** The user wants the daemon to start on boot.
  Whatever direction we pick has to answer "what project does the
  autostarted daemon assume?" cleanly.
- **Interaction with spec 0011's task system.** Tasks carry a
  `thread_id` already. If multi-thread arrives (option D), tasks
  naturally inherit their thread's project.
- **Voice ergonomics.** "Trabajemos en X" is a built-in intent that
  doesn't exist yet but would slot cleanly into the spec 0010
  dispatcher cascade.
- **Backwards compat.** Whatever lands, users who already rely on the
  "launch from project dir, Claude resumes that project" pattern
  should keep that working — either via PWD-fallback (option A/C) or
  via auto-detection ("I see you're in a git repo; should I attach
  to it?").

## Journal

- 2026-05-14: opened. User raised the observation during the spec
  0012 wrap-up, framed it as a thinking exercise for when we look at
  autostart-on-boot UX. Captured here so the consideration doesn't
  get lost.
