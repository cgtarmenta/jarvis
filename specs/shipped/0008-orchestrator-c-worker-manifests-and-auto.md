---
id: 0008
title: Orchestrator C — Worker manifests and autodiscovery
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
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

- [x] TOML manifest schema accepted from
      `~/.config/jarvis/workers/*.toml`. Fields: `id` (string,
      required, unique), `description` (string, optional),
      `command` (array of strings with placeholder support),
      `initial_command` (optional, used when `stateful=true` and no
      session id exists yet), `stateful` (bool, default false),
      `session_id_capture` (optional, see below), `async_eligible`
      (bool, default false), `tty` (bool, default false),
      `dispatch_hint` (string, optional).
      *Implemented in `src/workers/manifest.rs`. `WorkerManifest`
      with `#[serde(deny_unknown_fields)]` and `#[serde(default)]`
      for optional fields. Tests `parse_full_manifest` and
      `parse_minimal_manifest`.*
- [x] Placeholder substitution in `command` and `initial_command`:
      `{prompt}` (the resolved user query), `{session_id}` (the
      active session id for this worker on the current thread, if
      any), `{cwd}` (current working directory). Substitution is
      string-level; unmatched placeholders fail validation at
      manifest load time.
      *`KNOWN_PLACEHOLDERS` constant; `validate_placeholders`
      enforces the set at parse time; `scan_placeholders` and
      `substitute` are byte-safe over UTF-8. Tests
      `reject_unknown_placeholder`,
      `substitute_replaces_known_placeholders`,
      `substitute_passes_through_missing_value`,
      `substitute_preserves_non_ascii`, `scan_placeholders_basic`.*
- [x] `session_id_capture` schema for stateful workers:
      `{ source = "stdout" | "stderr", regex = "..." }`. The first
      regex match's first capture group is stored as the worker's
      new session id. v1 only supports stdout/stderr regex; other
      sources (env var, file path) are out of scope.
      *Regex compiled at `ManifestWorker::new`; bad regex disables
      the worker via the registry's load path. Both source values
      exercised end-to-end:
      `manifest_worker_captures_session_id_from_stdout`,
      `manifest_worker_captures_session_id_from_stderr`,
      `bad_capture_regex_disables_worker`.*
- [x] Autodiscovery at daemon startup: glob
      `~/.config/jarvis/workers/*.toml`, parse each, build an
      in-memory `WorkerRegistry`. Malformed manifests and
      manifests referencing binaries not on PATH log a warning
      and are *disabled* (not crash). The daemon starts
      successfully even if zero manifests are valid.
      *`WorkerRegistry::load_from_dir` never returns `Err`;
      every failure mode becomes a `DisabledWorker` entry with a
      human-readable reason. Tests
      `missing_dir_yields_empty_registry`,
      `malformed_toml_disables_worker`,
      `unknown_placeholder_disables_worker`,
      `missing_binary_disables_worker`,
      `bad_capture_regex_disables_worker`,
      `duplicate_id_disables_second_occurrence`,
      `good_and_bad_coexist`, `non_toml_files_are_ignored`.*
- [x] Built-in handlers (time, calc, spec management, session
      reset) self-register into the same `WorkerRegistry` at
      startup. They appear alongside external workers in
      `worker list` output and are addressable by id from the
      dispatcher (hija A). Built-in handlers do *not* live in
      `workers/*.toml`.
      *Mechanism shipped here: `WorkerRegistry::register_builtin`
      accepts an `Arc<dyn WorkerHandle>` and routes id collisions
      to the disabled pile with a "shadowed" reason. The
      handlers themselves (time/calc/spec/session-reset) are
      deferred to hija A — that's where the dispatcher cascade
      with stage-1 deterministic intents lives, and the
      handlers need that integration point to be useful. Tests
      `register_builtin_adds_handler`,
      `manifest_shadows_builtin_with_same_id`, plus the
      `format_built_in_active_worker` formatter test in
      `src/cli.rs`. Documented in the journal entry below.*
- [x] New CLI subcommand `jarvis worker list` prints all known
      workers (built-in + external) with status (`active` |
      `disabled: <reason>`). Output is human-readable; a future
      `--json` flag is out of scope.
      *Implemented as `Cmd::Worker { cmd: WorkerCmd::List }` with
      `format_worker_list(dir, &registry)` as a pure function for
      unit testability. Unit tests in `src/cli.rs#tests`:
      `format_empty_registry`, `format_built_in_active_worker`,
      `format_mixed_actives_and_disabled`. Integration test in
      `tests/cli.rs`: `worker_list_shows_bundled_claude_manifest`.
      Manual smoke against the user's real config produced the
      documented output shape.*
- [x] A starter manifest `workers/claude.toml` is shipped in
      `config/` (next to `config.example.toml`) that replicates
      the current claude-agent behavior exactly. Users who do
      nothing get the same Claude session attach they have today
      via the manifest path; the hard-coded
      `src/agents/claude.rs` becomes a thin shim that loads from
      the registry instead.
      *`config/workers/claude.toml` shipped. `STARTER_CLAUDE_MANIFEST`
      include_str'd; `config::ensure_workers_dir()` drops it
      into `~/.config/jarvis/workers/` on first run.
      `ClaudeAgent` refactored to `Arc<dyn WorkerHandle>`; falls
      back to the bundled manifest in-memory if the registry
      doesn't have it. Tests `bundled_starter_manifest_parses`,
      `warn_deprecated_handles_all_keys`. End-to-end smoke
      against the user's real `claude` binary confirmed the
      shim spawns claude and returns text identically to
      pre-refactor behaviour.*
- [x] Tests cover: parsing a valid manifest with all fields; a
      manifest with only required fields; rejection of duplicate
      ids; placeholder substitution including unmatched
      placeholder failure; the warn-and-disable path for missing
      binary; autodiscovery from a tempdir; built-in handler
      registration order.
      *Beyond the spec's bullet list: real-subprocess invocation
      tests for both `tty=false` and `tty=true` paths (sh/cat
      fixtures actually spawn), `JARVIS_VOICE_TURN=1` env
      propagation, exit-code error shapes for both pipes and PTY
      branches, and the bundled starter manifest parsing
      separately. Total: 27 unit tests across
      `src/workers/{manifest,registry,handle}.rs` plus the CLI
      tests cited above and one assert_cmd integration test.*

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

- 2026-05-14: shipped.

- 2026-05-14: shipped. Seven slices landed independently:

  * **C-1** (`ccaff88`) — `WorkerManifest` schema, placeholder
    substitution, UTF-8-safe scanner. 10 unit tests. No
    integration into existing code.
  * **C-2** (`d323d53`) — `WorkerRegistry` with autodiscovery
    from `~/.config/jarvis/workers/*.toml`; warn-and-disable
    for malformed manifests, unknown placeholders, missing
    binaries, duplicate ids, bad capture regex; alphabetical
    load order for reproducibility. 9 unit tests using
    tempdir fixtures. `regex = "1.12.3"` promoted from
    transitive dev-dep to direct dep.
  * **C-3** (`5ba2f11`) — `WorkerHandle` trait,
    `ManifestWorker` implementation with real subprocess
    spawning, `register_builtin` mechanism. Registry refactor
    to `Vec<Arc<dyn WorkerHandle>>`. 8 unit tests including
    actual `sh -c` subprocess invocations through the trait.
  * **C-4** (`c520708`) — `config/workers/claude.toml`
    bundled, auto-installed via `ensure_workers_dir()`.
    `src/agents/claude.rs` reduced to a shim over
    `Arc<dyn WorkerHandle>` with a bundled-manifest fallback
    when the registry doesn't have the claude entry.
    `[agent].options.{binary,system_prompt,extra_args,timeout}`
    deprecated with a warning; values are now ignored (option
    (a) from the user's refinement decision). `cwd` and
    `auto_resume` stay in `[agent].options` because they
    drive Jarvis-side attachment resolution. 2 unit tests +
    one end-to-end smoke test against the user's real
    `claude` binary.
  * **C-5** (`430bf75`) — `jarvis worker list` CLI subcommand.
    `format_worker_list(dir, &registry)` as a pure function
    for unit testability; `cmd_worker_list` is the thin
    I/O wrapper. 3 unit tests + 1 `assert_cmd` integration
    test + manual smoke against the user's real config.
  * **C-6** (`f2625e0`) — PTY spawn path via `portable-pty`.
    `ManifestWorker::invoke` becomes a two-branch dispatcher
    reading `self.manifest.tty`; common pre-spawn logic
    (placeholder values, template choice, prompt-in-argv
    detection) stays at the top. PTY combines stdout/stderr,
    so `session_id_capture::source` becomes a hint under
    `tty = true` — documented inline. 3 unit tests
    including a `sh -c 'tty; cat'` fixture that proves the
    spawned worker really runs inside a pseudo-terminal AND
    stdin round-trips.
  * **C-7** (this commit) — `JARVIS_VOICE_TURN=1` propagation
    test, stderr-source `session_id_capture` test, journal,
    and `## What` boxes ticked.

  Final test count: 85 unit + 5 integration = **90 passing**
  (started at 43 before C-1).

  Implementation tradeoffs that came up during the slices and
  are worth flagging for future readers:

  * **Built-in handler mechanism vs handler implementations.**
    The bullet asking for time/calc/spec/session-reset
    handlers to self-register was honoured at the *mechanism*
    level (`register_builtin` + shadowing rules) but not the
    *implementation* level — the actual handlers ship with
    hija A, where the dispatcher cascade lives. Without the
    cascade there's no callable surface for them to plug
    into. Splitting it this way keeps spec 0008 shippable
    without needing all of hija A's pipeline rewrite.
  * **PTY stream merging.** Under `tty = true`, the PTY
    delivers stdout + stderr on one device. The capture
    regex still runs (against the combined output), but the
    `source` field on `session_id_capture` becomes a hint
    rather than a hard route. Workers that genuinely need
    stream discrimination should stick with `tty = false`
    (the bundled `claude.toml` does).
  * **Bundled-manifest fallback in the claude shim.** The
    shim's `from_options` falls back to parsing
    `STARTER_CLAUDE_MANIFEST` in-memory if the registry
    doesn't have the claude entry — instead of erroring out.
    This matches the legacy ClaudeAgent's "warn at
    construction, fail at invoke" UX so the daemon never
    refuses to start because of a worker-dir issue.
  * **Deprecated agent options.** `system_prompt`,
    `extra_args`, `binary`, `timeout` in `[agent].options`
    are deprecated and ignored. A user who had these set
    will lose their customisation on first restart after
    the upgrade. The user's actual `config.toml` has none
    of them set, so no real-world breakage is expected;
    the warning fires for hypothetical users with overrides.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. First child of the orchestrator umbrella;
  blocks everything else because the manifest schema is what
  hijas A, B, D, E1, and E2 all reference.
