---
id:
title: setup wizard step for dispatcher.fallback
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
  - shipped/0013-orchestrator-b-llm-dispatcher-backends.md
---
# setup wizard step for dispatcher.fallback

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

- [ ] New `setup::dispatcher_fallback` step, run after the
      existing agent-configuration step and before the
      session/tasks steps. Confirm prompt:
      "Configure an LLM classifier for stage-2 routing?
      Defaults to off."
- [ ] On confirm, present a Select of available backends.
      Always include `openai_compat`. Include `oz` only if
      `which oz` succeeds (offering a backend the user
      can't run is friction).
- [ ] For `openai_compat`: collect `endpoint` (Input,
      required, validated as `http(s)://...` URL), `model`
      (Input, required), `api_key` (Password, optional,
      stored verbatim — same convention as the existing
      agent api-key prompts), `timeout_secs` (Input,
      optional, default 5). Skip the `headers` map — it's
      a power-user feature, document in the example TOML
      and let advanced users add it by hand.
- [ ] For `oz`: collect `model` (Input, required), with
      a free-form note that the model has to be one of the
      identifiers oz itself accepts (`oz agent list-models`
      or the docs). Skip `binary` and `timeout_secs` —
      defaults are right for 95% of installs; advanced
      users can edit the TOML.
- [ ] Validation before write: instantiate a temporary
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
- [ ] Write the resulting `[dispatcher.fallback]` section
      into the config TOML. The implementation will need
      to surface the section through `toml::Value` (the
      JarvisConfig field is `Option<toml::Value>` per
      0013 / B-4) so the round-trip is byte-stable across
      `serde::to_string` and the existing config-merge
      logic.
- [ ] A skip path: "leave stage 2 disabled" works at every
      branch, so an existing user re-running `setup` after
      ship doesn't lose their non-fallback config or get
      forced to configure something they don't want.
- [ ] On config-load failure path (the existing
      backup-and-regenerate flow), if the broken file had
      a `[dispatcher.fallback]` section, surface that fact
      to the user so they can paste it back into the
      regenerated file. Saves having to dig through the
      `.bak`.
- [ ] Tests cover: backend selection respects PATH
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

- 2026-05-14: opened. Came out of a session ship-review
  of spec 0013 — the user pointed out that the existing
  `jarvis setup` doesn't cover the new dispatcher
  fallback section and asked for parity. Scope is
  deliberately narrow: one wizard step, no new config
  shape, no plumbing changes outside `src/setup/`.
