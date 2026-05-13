---
id:
title: Voice-driven spec management
status: inbox
owner: unassigned
created: 2026-05-13
verifying:
related:
  - 0003  # depends on session continuity for multi-turn spec authoring
---

# Voice-driven spec management

## Why

Step 3 of the SDD plan. The scaffolding (this directory) gives us
markdown specs that humans and agents can read; the next step is letting
the user create / refine / promote them by voice. Without this, the
spec system is just static docs — useful, but not the "use Jarvis to
develop Jarvis" loop the user actually wants.

The user already has session continuity (spec 0003), so multi-turn
spec authoring is now feasible: "open a spec for X", *answer*, "add as
acceptance criteria Y", *confirmation*, "promote to active", *done*.

## What

Rough draft — needs sharpening before promotion to active.

- [ ] A new pipeline intent-detection step recognises voice phrases
      like "open a spec for ...", "abre un spec para ...", "promote
      spec NNNN", "ship NNNN" before forwarding to the agent.
- [ ] Recognised spec phrases invoke a `specs` module that owns the
      filesystem mutations — creating files in inbox, renaming with
      numeric IDs when promoting, moving between dirs.
- [ ] The agent is involved for the *content* (writing Why / What /
      How) but not for the *file operations* — that's deterministic
      Rust code so it can be tested.
- [ ] Voice "show me spec NNNN" reads back the title and acceptance
      criteria.
- [ ] CLI mirror commands: `jarvis spec new <title>`,
      `jarvis spec list`, `jarvis spec promote <NNNN>`,
      `jarvis spec ship <NNNN>`, `jarvis spec reject <NNNN> <reason>`.

## How

Open questions to resolve before promoting:

- **Intent detection.** Hardcoded regex / phrase match vs. asking the
  agent to classify? Hardcoded is reliable and offline; agent-based is
  more natural but adds a round-trip. Probably hardcoded for v1.
- **Content authoring.** When the user says "add as criterion Y", we
  need to insert Y into the right spec's `## What` section. Do we
  parse the markdown server-side, or hand the full spec + the user's
  request to the agent and ask it to return the new file? The latter
  is simpler but uses tokens.
- **Concurrent edits.** If the user runs `jarvis listen` and a daemon
  at once, two specs could collide on the next ID. Probably file-lock
  the spec dir at the OS level.

## Journal

- 2026-05-13: opened as part of step 2 of the SDD plan. Not committing
  to a design yet — leaving in inbox until we've used the manual flow
  for a few weeks and felt the real friction.
