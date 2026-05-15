---
id: 0014
title: Setup wizard step for dispatcher.fallback
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-15
verifying:
  - tests/setup_dispatcher_fallback.rs
related:
  - shipped/0013-orchestrator-b-llm-dispatcher-backends.md
---
# Setup wizard step for dispatcher.fallback

## Why

Spec 0013 (orchestrator B) shipped the stage-2 LLM
dispatcher behind a `[dispatcher.fallback]` config section.
Today the only way to enable it is to hand-edit
`~/.config/jarvis/config.toml`, cross-referencing the
example block at the tail of `config/config.example.toml`.
The rest of Jarvis's configuration surface — locale,
Whisper model, Piper voice, agent backend, wake word — is
configured interactively through `jarvis setup`, which
also handles dependency installation, validation, and
config-file backup-on-failure.

Two pain points this leaves on the table:

1. **Discoverability.** A user who runs `jarvis setup`
   doesn't learn that stage 2 even exists. The cascade
   feature is the headline value of the orchestrator
   umbrella; it shouldn't require reading the spec body
   or the example TOML to find.

2. **Correctness.** Hand-editing TOML against a strict
   schema is error-prone. The current soft-fail behaviour
   in spec 0013 means a typo'd `endpoint = ` or a missing
   `model = ` silently disables stage 2 with only a WARN
   log line — easy to miss. A wizard step validates input
   *before* writing, so the user gets immediate feedback
   ("that URL doesn't look like a `/chat/completions`
   path") instead of a quiet degradation at runtime.

Specifically for the user's setup: with VPN access to a
GB200 Triton cluster and oz already installed locally,
both backends are realistic picks — the wizard should
let them pick one in a few keystrokes.

## What

- [x] New `setup::dispatcher_fallback` step, run after the
      existing agent-configuration step and before the
      session/tasks steps. Confirm prompt:
      "Configure an LLM classifier for stage-2 routing?
      Defaults to off."
- [x] On confirm, present a Select of available backends.
      Always include `openai_compat`. Include `oz` only if
      `which oz` succeeds (offering a backend the user
      can't run is friction).
- [x] For `openai_compat`: collect `endpoint` (Input,
      required, validated as `http(s)://...` URL), `model`
      (Input, required), `api_key` (Password, optional,
      stored verbatim — same convention as the existing
      agent api-key prompts), `timeout_secs` (Input,
      optional, default 5). Skip the `headers` map — it's
      a power-user feature, document in the example TOML
      and let advanced users add it by hand.
- [x] For `oz`: collect `model` (Input, required), with
      a free-form note that the model has to be one of the
      identifiers oz itself accepts (`oz model list`).
      Skip `binary` and `timeout_secs` — defaults are right
      for 95% of installs; advanced users can edit the TOML.
- [x] Validation before write: instantiate a temporary
      `OpenAiCompatBackend` / `OzCliBackend` with the
      collected values and run a single classify call
      against a trivial fixture ("hello world" with a
      one-worker list). Surface success / failure to the
      user as a "✓ classifier reachable" / "⚠ classifier
      didn't respond — saving config anyway, you can fix
      it later" line. **Do not block save on validation
      failure** — the endpoint may come online later and
      a hard refusal would re-create the existing
      brittleness around required services.
- [x] Write the resulting `[dispatcher.fallback]` section
      into the config TOML. The implementation will need
      to surface the section through `toml::Value` (the
      JarvisConfig field is `Option<toml::Value>` per
      0013 / B-4) so the round-trip is byte-stable across
      `serde::to_string` and the existing config-merge
      logic.
- [x] A skip path: "leave stage 2 disabled" works at every
      branch, so an existing user re-running `setup` after
      ship doesn't lose their non-fallback config or get
      forced to configure something they don't want.
- [x] On config-load failure path (the existing
      backup-and-regenerate flow), if the broken file had
      a `[dispatcher.fallback]` section, surface that fact
      to the user so they can paste it back into the
      regenerated file. Saves having to dig through the
      `.bak`.
- [x] Tests cover: backend selection respects PATH
      detection, validation success path, validation
      failure path (still writes config), skip path,
      round-trip of the resulting TOML through
      `config::load` produces an equivalent
      `JarvisConfig::dispatcher::fallback`.

## How

Implementation notes:

- New module `src/setup/dispatcher.rs` mirroring the shape
  of `src/setup/voices.rs` / `src/setup/whisper.rs` —
  small focused file, no cross-module state.
- The Select for backends uses `dialoguer::Select` with
  labels `"OpenAI-compatible HTTP endpoint"` and `"oz
  (Warp's CLI)"`. The latter only appears when `which::which("oz")`
  resolves; otherwise we'd offer something that errors
  on first classify.
- Endpoint URL validation: cheap heuristic — must parse
  with `url::Url::parse` (already a transitive dep of
  ureq) and have scheme http/https. We don't try to
  enforce the `/chat/completions` suffix because Triton's
  per-model routes embed the model name and break that
  rule legitimately.
- The "live validation" call uses the same `LlmBackend`
  trait as production; the fixture is
  `default_classifier_prompt("hello", &[WorkerInfo{ id:
  "test", dispatch_hint: None }])` and we ignore the
  returned worker id (we only care that the call returned
  Ok). Timeout is the user-configured value, defaulting
  to 5s; the wizard pauses for at most that long.
- TOML serialisation: the existing setup wizard already
  writes TOML by deserialising the existing file,
  mutating the struct, and re-serialising. The
  `dispatcher.fallback` field is `Option<toml::Value>`
  so we build a `toml::Table` programmatically and wrap
  it in `Some(Table(...))`. Confirm this round-trips
  cleanly through `toml::to_string_pretty` before
  shipping.
- The existing config-load-failure path already shows
  the raw error string; we just need to grep that string
  for `"dispatcher.fallback"` and emit a tip if found.
  No structural change to error handling required.

Out of scope:

- A separate `jarvis dispatcher status` command
  surfacing which stage-2 backend is wired in at runtime.
  Useful but orthogonal — file as a future spec when
  there's a real debugging need.
- Hot-reloading the config without daemon restart. Not
  what setup is for.
- Choosing between multiple oz models via a Select. The
  list of available models depends on the user's Warp
  account / installed model packs; we can't enumerate
  it reliably. Free-text Input with a doc link is the
  right v1.
- Anthropic-OpenAI-compat / Groq / Triton presets as
  Select options. Tempting, but the URL surface is the
  config and a curated dropdown rots faster than the
  free-text equivalent. Document the common cases in
  the example TOML instead.

## Journal

- 2026-05-15: shipped. Manual verification on the user's CachyOS
  install passed: backend Select hides oz when missing / shows it
  when present; openai_compat URL validator catches non-http
  inputs inline; live probe surfaces ✓/⚠ without blocking save;
  oz model table is multi-column with Tab-completion; agent step
  no longer asks for `WARP_API_KEY` when `oz whoami` succeeds;
  TOML round-trip preserves `[dispatcher.fallback]` across
  re-runs of `jarvis setup` (the regression fix in save_config).
  All acceptance criteria met; 259 unit + 10 integration tests
  green; cargo fmt clean.

- 2026-05-15: replaced the single-column `Select` for oz models
  with a multi-column table + Input/Tab-completion. User flagged
  the 50-line "chorizo" as bad UX. Dialoguer doesn't do
  multi-column natively (single col forever), so the fix is to
  (a) pre-render the catalog as a column-major table that fits
  the live terminal width via `console::Term::stdout().size()`,
  (b) prompt with `Input::completion_with(&ModelCompletion)`
  where `ModelCompletion` does shell-style completion (unique
  prefix → full id, multi-match → longest common prefix). `auto`
  stays the default. Free-text fallback is now implicit (Input
  accepts anything), removing the `Other (type custom)…`
  sentinel. `console` promoted from transitive dep to direct.
  Seven new unit tests cover `format_models_table` (column
  packing, narrow-terminal-forces-1-col, empty), `ModelCompletion`
  (unique/multi/no-match), and `longest_common_prefix`.

- 2026-05-15: tag-along fix to the agent step (technically
  pre-existing, surfaced during manual test of 0014). When
  the user picks `warp` as the agent, the wizard used to
  ask for `WARP_API_KEY` unconditionally — but oz's primary
  auth path is `oz login` (cached session token), and a
  logged-in user doesn't need a key at all. New
  `configure_warp_auth` probes `oz whoami` (exit 0 = logged
  in) and skips the prompt with `✓ oz is already logged in
  — no API key needed.` On failure, prompt text changes to
  recommend `oz login` over storing the key. Helper
  `oz_is_authenticated` is a thin Command-status wrapper —
  no tests because mocking subprocess auth is more harmful
  than helpful (the probe is deterministic from oz's exit
  code).

- 2026-05-15: dynamic oz model Select. Reversed the spec's
  "Out of scope: choosing between multiple oz models via a
  Select" decision after the user pointed out the
  maintainability tax of stale hint text (the in-code example
  `claude-3.7-sonnet` was already gone from oz's catalog).
  The original "we can't enumerate reliably" claim turned out
  to be wrong: `oz model list --output-format json` emits a
  clean `[{"id":"…"},…]` shape we parse with serde_json. New
  surface in `src/setup/dispatcher.rs`:
  `fetch_oz_models()` (spawns the subprocess) +
  `parse_oz_models_json(&str) -> Result<Vec<String>>`
  (separated for unit-testability). `collect_oz` first tries
  the live list and Selects from it with `auto` as the
  default; the last option `Other (type custom)…` falls
  through to the original free-text Input so private/
  pre-release models still work. Hard fetch failures (binary
  missing, not authenticated, network) print a soft warning
  and degrade to free-text — the wizard never blocks on the
  list. Five new unit tests cover the JSON parse contract
  (real shape, extra fields, empty-id filter, garbage,
  empty array). Pushed back on a related ask to add
  runtime auto-fallback in `OzCliBackend` ("if model gone,
  call `auto` instead"); the dynamic Select solves the
  config-time problem and a silent runtime swap would
  mask a real misconfig with an unannounced cost/latency
  profile change. The existing cascade soft-fail (stage 2
  errors → stage 3 takes over) already provides the
  graceful-degradation envelope.

- 2026-05-15: post-manual-test polish. Two real findings from the
  user's first end-to-end run:
  (a) The subcommand hint was wrong — `oz agent list-models`
  doesn't exist; the correct one is `oz model list`. Fixed in
  `collect_oz` and the example model id moved to
  `claude-4-6-sonnet-high` (current per oz's actual catalog;
  `claude-3.7-sonnet` is gone).
  (b) Real bug: dialoguer's `Input` doesn't trim, so a pasted
  ` qwen-3.6-plus-fireworks` (leading space from copying off
  oz's table output) made the live probe fail with
  `Unknown model id`. Added `normalize_user_input(&str)` and
  routed every text field (endpoint, model, api_key,
  timeout) through it. New unit + integration tests pin the
  trim and the URL validator's agreement with trimmed input.
  The probe surfaced this correctly — verbose error included
  the valid model list — so the design held; the fix just
  prevents the legitimate id getting rejected for whitespace.

- 2026-05-15: implementation landed. `src/setup/dispatcher.rs`
  mirrors `voices.rs` / `whisper.rs` shape; the new step is
  wired between agent and save in `setup::run`. Surfaced a
  pre-existing regression while doing the wiring: the wizard's
  `save_config` didn't serialise `session`, `tasks`, or
  `dispatcher` — so a wizard-written `[dispatcher.fallback]`
  would have been wiped on the next `jarvis setup`. Fixed by
  extending the renderer (renamed to `render_config`, returns
  String) and adding `serialize_dispatcher_fallback` that wraps
  the inner table back under `dispatcher.fallback` so the toml
  crate emits the dotted parent header (and any sub-tables
  like `headers` get the full `[dispatcher.fallback.headers]`
  prefix). URL validator is a hand-rolled scheme check — the
  spec suggested `url::Url::parse` via a transitive dep, but
  the simple `starts_with` check covers the same accept set
  without leaning on a non-declared crate. Integration tests
  in `tests/setup_dispatcher_fallback.rs` cover URL accept/
  reject, the section-emit regression, both backends'
  render→load→`build_llm_stage` round-trip, the
  nested-headers full-path emit, the skip path, and the
  load-failure hint trigger. Suite: 247 + 8 new tests, all
  green; cargo fmt clean.

- 2026-05-15: promoted to `active/` as **0014**. No design
  changes — the inbox draft already had concrete bullets,
  validation strategy, and module placement. Title and
  filename normalised (kebab-slug `setup-wizard-dispatcher-
  fallback`); `verifying:` seeded with the planned
  `tests/setup_dispatcher_fallback.rs` integration file.
  Owner stays `unassigned` per repo convention.

- 2026-05-14: opened. Came out of a session ship-review
  of spec 0013 — the user pointed out that the existing
  `jarvis setup` doesn't cover the new dispatcher
  fallback section and asked for parity. Scope is
  deliberately narrow: one wizard step, no new config
  shape, no plumbing changes outside `src/setup/`.
