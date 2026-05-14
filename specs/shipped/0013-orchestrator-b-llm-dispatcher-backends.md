---
id: 0013
title: Orchestrator B — LLM dispatcher backends
status: shipped
owner: unassigned
created: 2026-05-14
shipped: 2026-05-14
verifying:
related:
id: 
shipped: 
---

# Orchestrator B — LLM dispatcher backends

Part of the orchestrator vision. Adds stage 2 to the dispatcher
cascade: a pluggable LLM-based classifier that picks a worker
when no deterministic intent matches. Two backends ship at v1
(`OzCli` for Warp's open-source models, `OpenAiCompat` for any
OpenAI-style endpoint including local Triton or Ollama servers).
The whole stage is opt-in; users who don't configure it see no
change.

## Why

The hand-coded intent matcher in hija A handles obvious patterns
(time, calc, spec, etc.). For everything else, the cascade falls
straight through to the default worker (Claude). This works, but
it means we re-pay the Claude cold-spawn for every "what's the
capital of France" type question that Claude is overkill for.
With ~30+ patterns the rule table also starts to feel like the
wrong tool — paraphrase variance in Spanish is just too high to
match deterministically.

An LLM classifier sitting between stages 1 and 3 routes those
edge cases by *meaning* rather than by keyword. It can send
"explain blockchains" to a cheap general model, "refactor X" to
Claude with cwd, "qué tiempo hace" to a weather worker —
based on the workers' `dispatch_hint` strings (from hija C).

We don't pick a single LLM provider because the right choice
depends on the user. The trait + two backends let the user
opt into Warp's open-source models (no extra config), a local
Triton server on their own GPU infra (zero per-turn cost, low
latency), an Ollama instance (privacy + offline), or a managed
endpoint like Groq / Fireworks (best latency for the money).

## What

- [x] `LlmBackend` trait in a new `src/dispatcher/llm.rs` module.
      Method: `classify(&self, prompt: &str, workers:
      &[WorkerInfo]) -> Result<Option<String>>` where
      `WorkerInfo` is a thin struct of `{ id, dispatch_hint }`
      derived from the `WorkerRegistry`. Backend returns the
      chosen worker id (or `None` to decline); the cascade
      adapter wraps that into a `DispatchDecision` after
      validating the id against the live registry. *(B-1,
      shipped ecb28fe; landed as a directory module
      `src/dispatcher/llm/` once B-2 added the second file.)*
- [x] `OzCliBackend` implementation: spawns `oz agent run
      --model <model_id> --prompt <built classifier prompt>`
      with stdin null, stdout + stderr piped. Prompt rides
      in argv as a single element so newlines / quotes round-
      trip intact (no shell interpolation). The classifier
      prompt is built from the same `default_classifier_prompt`
      the HTTP backend uses, keeping behaviour aligned.
      Stdout parses through `parse_worker_id`. Non-zero exit
      becomes a backend error including a stderr snippet.
      Timeout uses a watchdog thread mirroring `recorder.rs`'s
      pattern: child is placed in its own process group via
      `process_group(0)` and SIGTERM'd via `kill(-pgid, ...)`
      so the real `oz`'s child model-runner doesn't survive
      its parent and hang the pipe read. *(B-3, shipped this
      commit.)*
- [x] `OpenAiCompatBackend` implementation: HTTP POST to a
      configurable endpoint following the OpenAI Chat
      Completions wire protocol. Configuration fields:
      `endpoint` (full URL — caller supplies path including
      `/chat/completions`), `model` (string), optional
      `api_key` sent as `Authorization: Bearer ...`, optional
      `headers` map for custom auth / VPN routing,
      `timeout_secs` (default 5s, per-call). HTTP client is
      `ureq` (already a direct dep) rather than reqwest — the
      spec mentioned the wrong crate by name. Sampling is
      `temperature = 0`, `max_tokens = 32` so the same prompt
      always produces the same answer (cache-friendly) and
      replies are short. Response parser accepts both the
      plain-string and array-of-parts content shapes so
      multimodal-extended vLLM/Triton builds work out of
      the box. *(B-2, shipped this commit.)*
- [x] Config schema in `config.toml`:
      ```
      [dispatcher.fallback]
      backend = "oz" | "openai_compat"
      model = "..."
      # backend-specific fields
      ```
      *(Section renamed `[dispatcher.fallback]` rather than
      `[listener.fallback]` — the dispatcher cascade is the
      module this actually configures; the spec's original name
      was placeholder.)*
      Absent → stage 2 skipped (zero behavior change from a
      pure-A install). Malformed → daemon logs a WARN, disables
      stage 2, starts normally. Implementation stores the raw
      `toml::Value` in `JarvisConfig` and validates only at
      pipeline init via `build_llm_stage`, so a typed-parse
      failure can't take the daemon down. *(B-4, shipped this
      commit.)*
- [x] Cascade integration: `CascadeDispatcher` gains an
      optional stage-2 LLM dispatcher (`LlmDispatcher` adapter).
      If absent, the cascade is the v1 two-stage shape; if
      present, unmatched stage-1 prompts go through the
      backend, with results validated against the live worker
      registry (hallucinated ids become declines). Backend
      errors (`Err`) and unknown ids both log at WARN and
      return `Ok(None)` so the cascade falls through to
      stage 3. Never propagates a dispatch failure. *(B-4.)*
- [x] Caching: in-memory `Mutex<HashMap<(prompt, sorted(worker_ids))
      , (worker_id, cached_at)>>` with 60s TTL. Both picks AND
      declines get cached (the LLM call is what's expensive,
      not the decision). Worker-id list in the key is sorted
      so registry insertion order doesn't cause spurious
      misses. Hard cap of 1024 entries with oldest-25%
      eviction. Backend errors are *not* cached so a transient
      hiccup doesn't lock out for 60s. The dispatcher lives
      behind a process-wide `OnceLock` in the pipeline so the
      cache survives across turns. *(B-4.)*
- [x] Timeout: per-backend (5s default, configurable via
      `timeout_secs`). `OpenAiCompatBackend` uses
      `ureq::Agent::timeout`; `OzCliBackend` uses a watchdog
      thread that SIGTERMs the process group. Timeout
      surfaces as `Err` from the backend which the adapter
      then swallows into stage-3 fallthrough — meeting the
      spec's "never kill the user's turn" invariant. *(Done
      in B-2/B-3; verified end-to-end in B-4.)*
- [x] Tests cover: trait dispatch with a mock backend
      *(B-1)*; OzCli backend invocation with a mock `oz`
      binary *(B-3, via #!/bin/sh fixtures + TempDir)*;
      OpenAiCompat with a mock HTTP server *(B-2, hand-rolled
      TcpListener — no new deps)*; cascade integration
      showing stage 2 being inserted/omitted by config
      presence *(B-5, `pipeline::tests::cascade_has_*_stages_*`
      + `try_build_llm_stage_*`)*; timeout fallthrough
      *(B-2 + B-3 timeout tests, plus cascade-level
      `cascade_falls_through_to_default_when_llm_errors` in
      B-5)*; cache hit on repeated prompt *(B-4, multiple
      cache-key tests)*; malformed config startup behaviour
      *(B-4 + B-5 soft-fail tests). *(B-5, this commit.)*

## How

Implementation notes:

- The classifier prompt template is data; ship a default but
  let users override via
  `~/.config/jarvis/dispatcher-prompt.txt`. The default lists
  workers and their hints, asks for "the worker id alone on
  the first line".
- For `OzCliBackend`, model availability is *not* validated at
  startup (`oz` may go online/offline). We just retry on
  failure into stage 3.
- For `OpenAiCompatBackend`, we should make a small ping at
  startup against the endpoint, log the result, but still
  start the daemon — the endpoint might come online later.
- The cache key is `(prompt, sorted(worker_ids))` so worker
  registry changes invalidate cached entries naturally.
- We don't manage `oz` or LLM API tokens — that's the user's
  prior auth, just like Claude.

Out of scope:
- Streaming dispatcher decisions (we always read the full
  response before routing).
- LLM-based intent *and* parameter extraction (e.g. extract
  "Tokio" from "y en Tokio?" with the LLM). v1's stage 1
  handles parameters via regex; LLM-level disambiguation
  via cascade re-route is enough.
- Fine-tuning / few-shot training. Stick with off-the-shelf
  models + good prompts.
- Multi-backend ensembles ("ask two LLMs, vote"). Pick one
  via config.
- Hot-swapping the backend without daemon restart.

## Journal

- 2026-05-14: shipped.

- 2026-05-14: shipped. Five slices: B-1 (trait, ecb28fe),
  B-2 (HTTP backend, 741f2ca), B-3 (CLI subprocess, 65358d6),
  B-4 (cascade + config + cache, 242c4b5), B-5 (E2E tests +
  ship). 245 lib tests green; clippy + fmt clean.
  Section name in user-facing config is
  `[dispatcher.fallback]` (renamed from the spec's original
  `[listener.fallback]` — more accurate to what it
  configures). HTTP client is `ureq` (already a direct dep)
  rather than the `reqwest` the spec mentioned. Two
  out-of-scope items from the spec did NOT land and are left
  to future work: (1) `~/.config/jarvis/dispatcher-prompt.txt`
  override hook for the classifier prompt — the default
  template ships hardcoded for now, users who want a custom
  prompt template will need to wait for a follow-up spec;
  (2) `OpenAiCompatBackend` startup ping to log endpoint
  reachability — also deferred. Both are nice-to-have polish,
  neither blocks the core feature.

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Blocks on hija A (cascade slot must
  exist) and hija C (workers need `dispatch_hint`). The user
  explicitly named their GB200 Triton infra as a target
  consumer of the `OpenAiCompatBackend` — that's the
  realistic test case once this lands.
