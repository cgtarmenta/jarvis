---
id: 0013
title: Orchestrator B — LLM dispatcher backends
status: active
owner: unassigned
created: 2026-05-14
shipped:
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

- [ ] `LlmBackend` trait in a new `src/dispatcher/llm.rs` module.
      Method: `classify(&self, prompt: &str, workers:
      &[WorkerInfo]) -> Result<DispatchDecision>` where
      `WorkerInfo` is a thin struct of `{ id, dispatch_hint }`
      derived from the `WorkerRegistry`. The backend returns a
      `DispatchDecision` exactly like hija A's stage-1
      dispatchers — same downstream code path.
- [ ] `OzCliBackend` implementation: spawns `oz agent run
      --model <model_id> --prompt <built classifier prompt>`,
      reads stdout, parses out the chosen worker id. The
      classifier prompt is built from a small Jinja-style
      template (or just `format!` — we don't need a templating
      crate for this) that includes the workers + hints + the
      user's transcript and asks for the worker id as the
      first whitespace-delimited token on stdout.
- [ ] `OpenAiCompatBackend` implementation: HTTP POST to a
      configurable endpoint following the OpenAI Chat
      Completions wire protocol. Configuration fields:
      `endpoint` (URL), `model` (string), optional `api_key`
      (secret), optional `headers` (string→string map for
      custom auth / VPN routing), `timeout_secs` (default 5).
      Request uses JSON mode if the endpoint supports it for
      schema-constrained output. Reqwest is the HTTP client
      (we already pull it transitively); add only if not
      already in `Cargo.toml`.
- [ ] Config schema in `config.toml`:
      ```
      [listener.fallback]
      backend = "oz" | "openai_compat"
      model = "..."
      # backend-specific fields
      ```
      If `[listener.fallback]` is absent, stage 2 is skipped
      (zero behavior change from a pure-A install). If
      `[listener.fallback]` is malformed at startup, the daemon
      logs a warning, disables stage 2, and starts normally
      (no crash on bad config).
- [ ] Cascade integration: `CascadeDispatcher` (from hija A)
      gains a stage-2 slot. If the slot is empty, the cascade
      behaves exactly as v1 of hija A. If filled, an unmatched
      stage-1 prompt is passed to the LLM backend, whose
      result enters stage 3 as a `DispatchDecision`. If the
      LLM backend errors (timeout, network, malformed
      response), stage 3 takes over with the default worker.
      *Never* let a dispatcher error kill the user's turn.
- [ ] Caching: identical prompts within a 60-second window
      bypass the LLM call and reuse the cached decision. Cache
      is in-memory, per-thread. Keeps cost down on repeated
      questions and is essentially free to implement.
- [ ] Timeout: backend calls have a per-call timeout (default
      5s, configurable). On timeout, fall through to stage 3
      with a debug log entry. No retry on timeout — speed
      matters more than precision here.
- [ ] Tests cover: trait dispatch with a mock backend; OzCli
      backend invocation with a mock `oz` binary; OpenAiCompat
      with a mock HTTP server; cascade integration showing
      stage 2 being inserted/omitted by config presence;
      timeout fallthrough; cache hit on repeated prompt;
      malformed config startup behaviour.

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

- 2026-05-14: promoted to active.

- 2026-05-14: opened. Blocks on hija A (cascade slot must
  exist) and hija C (workers need `dispatch_hint`). The user
  explicitly named their GB200 Triton infra as a target
  consumer of the `OpenAiCompatBackend` — that's the
  realistic test case once this lands.
