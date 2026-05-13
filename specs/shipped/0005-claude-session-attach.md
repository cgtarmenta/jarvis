---
id: 0005
title: Claude session attach
status: shipped
owner: tadeo
created: 2026-05-13
shipped: 2026-05-13
verifying:
  - cargo run -- claude sessions
  - cargo run -- claude attach --latest --cwd /tmp/fake-repo
  - cargo test --lib agents::claude_attach
related:
  - 0001
  - 0003
  - 0004
id: 
shipped: 
---

# Claude session attach

## Why

The user often has a live Claude Code session running in some other
repo. Today, voice-talking to Jarvis means talking to a *fresh* Claude
turn with no knowledge of that conversation. The work the user is in the
middle of (file edits, tool calls, design decisions) is invisible to
Jarvis.

Quote from the original brief:

> *"Mañana estoy trabajando en otro repo en CLI, y quiero que jarvis
> continue mientras hago otra cosa..."*

Claude Code already records each session as a JSONL transcript under
`~/.claude/projects/<encoded-path>/<uuid>.jsonl` and supports
`claude --print --resume <uuid>` to continue a session from the CLI.
The piece Jarvis is missing is plumbing: telling its Claude agent which
UUID to resume, plus a UX layer (CLI + voice) for picking one.

Two delivery modes:

1. **Auto-resume**: with `[agent].auto_resume = true` and a `cwd` set,
   each turn picks the newest JSONL in that project's session dir.
   Works for "I'm in repo X all day; always continue whatever I was
   doing there".
2. **Pinned attach**: `jarvis claude attach <uuid>` (or
   `--latest [--cwd path]`) writes the choice to a small state file
   that overrides config for the current session. Works for
   "right now I specifically want to continue UUID Y".

Honest caveat we'll document but not solve: if the user is *also*
typing in a terminal Claude that uses the same session, both writers
append turns to the same `.jsonl`. Claude Code tolerates the
intercalation but the conversation becomes a hybrid. We surface this
in `jarvis claude sessions` output rather than try to lock the file.

## What

- [x] `[agent].auto_resume` (bool, default `false`) added to the
      `claude` agent. When `true`, the agent picks the newest session
      JSONL in the project's session dir on every turn and passes
      `--resume <uuid>` to `claude --print`.
- [x] `[agent].cwd` already exists and is honoured as both Claude's
      working dir AND the basis for the auto-resume project path.
      No new field needed.
- [x] A pinned-attachment state file at
      `$XDG_CACHE_HOME/jarvis/claude-attach.toml` (single
      `session_id = "..."` or `auto_resume = true` + `cwd = "..."`)
      overrides `[agent]` config for the current session.
- [x] `jarvis claude sessions` lists every session under
      `~/.claude/projects/` newest-first, grouped by project, with
      mtime + size + first user message preview. Supports
      `--cwd <path>` to filter to one project.
- [x] `jarvis claude attach <uuid>` writes the state file pinning that
      UUID; subsequent `jarvis listen` / daemon turns resume it.
- [x] `jarvis claude attach --latest [--cwd <path>]` writes
      `auto_resume = true` + cwd to the state file.
- [x] `jarvis claude detach` deletes the state file (turn returns to
      whatever `[agent]` config says — `auto_resume` or stateless).
- [x] Doctor reports the current attachment (path of attached JSONL or
      "stateless") so the user can see at a glance what session voice
      will hit.
- [x] CHANGELOG entry under `## [Unreleased]`.

## How

ClaudeAgent constructor reads three sources in priority order:

1. State file `~/.cache/jarvis/claude-attach.toml` (if exists)
2. `[agent].session_id` / `[agent].auto_resume` from config
3. Nothing → stateless (current behaviour)

A helper `claude_attach::resolve()` returns a small enum:

```rust
enum Attachment {
    None,
    Pinned(String),       // explicit uuid
    Latest { cwd: PathBuf } // resolve newest JSONL at respond() time
}
```

The `respond()` method translates that into a `--resume <uuid>` argv
prefix when applicable.

Path encoding (`/home/foo/bar` → `-home-foo-bar`) is reverse-engineered
from inspecting `~/.claude/projects/` on this machine. We isolate it
behind `claude_attach::encode_project_path(path)` so a future Anthropic
change has one place to patch.

State file format is intentionally minimal so a user can `cat` and edit
it by hand:

```toml
# Either:
session_id = "c47a097d-9d37-422d-b5ea-140d9092bfcc"
# Or:
auto_resume = true
cwd = "/home/dat30/github/foo"
```

Voice intents — three new patterns in `specs::intent` (renamed
appropriately) or a separate `claude::intent`:

  - "attach to the latest claude session" / "atacha al ultimo claude"
  - "attach to session <uuid-prefix>" — match by prefix
  - "detach" / "ya no atacha"

For v1 these go through `specs::intent` since that's where the prefix
matcher already lives — moving them out is fine when we add a third
domain.

## Journal

- 2026-05-13: shipped.

- 2026-05-13: promoted to active.

- 2026-05-13: opened during SDD walkthrough.
- 2026-05-13: sharpened acceptance criteria. Picked state-file design
  over mutating `[agent]` config so the persistent intent
  (`auto_resume = true` always) stays cleanly separate from ephemeral
  attachment ("right now use UUID Y"). Ready to promote.
