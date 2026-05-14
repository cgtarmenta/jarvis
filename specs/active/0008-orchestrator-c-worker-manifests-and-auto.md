---
id: 0008
title: Orchestrator C — Worker manifests and autodiscovery
status: active
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
id: 
shipped: 
---

# Orchestrator C — Worker manifests and autodiscovery

Part of the orchestrator vision (see umbrella spec for context).
This is the foundation: defines *how* a worker is declared so every
other child spec has something concrete to reference. No
user-visible behavior change ships with this alone — it sets up the
architecture for everything that follows.

## Why

Today Jarvis has exactly one "agent": Claude Code via `claude
--print --resume <uuid>`, hard-coded in `src/agents/claude.rs`.
Adding another agent (gemini-cli, oz, a custom local script) means
a Rust diff plus a release. The orchestrator vision requires that
adding a worker be a config change, not a code change — both for
the user (so they can plug in their own tooling) and for us (so
adding a new agent doesn't bottleneck on a build).

The declarative format also gives the LLM dispatcher (hija B)
something to enumerate when classifying utterances ("here are the
workers I know about, with their dispatch_hint descriptions, pick
one"). Without manifests, that dispatcher has nothing to choose
from.

## What

- [ ] TOML manifest schema accepted from
      `~/.config/jarvis/workers/*.toml`. Fields: `id` (string,
      required, unique), `description` (string, optional),
      `command` (array of strings with placeholder support),
      `initial_command` (optional, used when `stateful=true` and no
      session id exists yet), `stateful` (bool, default false),
      `session_id_capture` (optional, see below), `async_eligible`
      (bool, default false), `tty` (bool, default false),
      `dispatch_hint` (string, optional).
- [ ] Placeholder substitution in `command` and `initial_command`:
      `{prompt}` (the resolved user query), `{session_id}` (the
      active session id for this worker on the current thread, if
      any), `{cwd}` (current working directory). Substitution is
      string-level; unmatched placeholders fail validation at
      manifest load time.
- [ ] `session_id_capture` schema for stateful workers:
      `{ source = "stdout" | "stderr", regex = "..." }`. The first
      regex match's first capture group is stored as the worker's
      new session id. v1 only supports stdout/stderr regex; other
      sources (env var, file path) are out of scope.
- [ ] Autodiscovery at daemon startup: glob
      `~/.config/jarvis/workers/*.toml`, parse each, build an
      in-memory `WorkerRegistry`. Malformed manifests and
      manifests referencing binaries not on PATH log a warning
      and are *disabled* (not crash). The daemon starts
      successfully even if zero manifests are valid.
- [ ] Built-in handlers (time, calc, spec management, session
      reset) self-register into the same `WorkerRegistry` at
      startup. They appear alongside external workers in
      `worker list` output and are addressable by id from the
      dispatcher (hija A). Built-in handlers do *not* live in
      `workers/*.toml`.
- [ ] New CLI subcommand `jarvis worker list` prints all known
      workers (built-in + external) with status (`active` |
      `disabled: <reason>`). Output is human-readable; a future
      `--json` flag is out of scope.
- [ ] A starter manifest `workers/claude.toml` is shipped in
      `config/` (next to `config.example.toml`) that replicates
      the current claude-agent behavior exactly. Users who do
      nothing get the same Claude session attach they have today
      via the manifest path; the hard-coded
      `src/agents/claude.rs` becomes a thin shim that loads from
      the registry instead.
- [ ] Tests cover: parsing a valid manifest with all fields; a
      manifest with only required fields; rejection of duplicate
      ids; placeholder substitution including unmatched
      placeholder failure; the warn-and-disable path for missing
      binary; autodiscovery from a tempdir; built-in handler
      registration order.

## How

Implementation notes:

- `WorkerManifest` struct deserialised by serde from TOML.
  Validation happens after deserialisation (the regex is
  compiled, placeholders in `command` are checked against the
  known set).
- `WorkerRegistry` is built once at daemon start, passed by
  shared reference to the pipeline. No hot-reload of manifests
  in v1.
- `WorkerHandle` is the runtime representation: a `WorkerManifest`
  plus a way to spawn it with substituted placeholders. Built-in
  handlers implement the same `WorkerHandle` trait so the
  dispatcher doesn't have to branch between built-in and external.
- Spawn mechanics (PTY vs plain pipes) read the manifest's `tty`
  field. `portable-pty` is the leading crate candidate; the PTY
  glue is part of this spec because the manifest schema requires
  it as a field.
- The migration of `src/agents/claude.rs` to a manifest-loaded
  shim is the last step. The Agent trait stays; the build
  function reads from the registry instead of hard-coding
  ClaudeAgent.

Out of scope:
- Plugin-style worker handshake (no `--describe` protocol).
- Hot reload of manifests (restart the daemon to pick up
  changes).
- Per-user/per-machine manifest overrides (we autodiscover one
  directory; users can edit it).
- Manifest signing / verification.

## Journal

- 2026-05-14: promoted to active.

- 2026-05-14: opened. First child of the orchestrator umbrella;
  blocks everything else because the manifest schema is what
  hijas A, B, D, E1, and E2 all reference.
