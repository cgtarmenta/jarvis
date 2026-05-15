---
id:
title: Additional stage-2 backends — codex / gemini / future
status: inbox
owner: unassigned
created: 2026-05-15
shipped:
verifying:
related:
  - shipped/0013-orchestrator-b-llm-dispatcher-backends.md
  - shipped/0014-setup-wizard-dispatcher-fallback.md
  - inbox/2026-05-15-stage-2-fix-oz-add-opencode.md
---

# Additional stage-2 backends — codex / gemini / future

> **Umbrella spec, intentionally remains in `inbox/` as a vision
> reference.** Promote individual children to active when a real
> user need surfaces. Reject the umbrella only if we decide the
> stage-2 backend surface won't grow beyond `oz` + `opencode` +
> `openai_compat`.

## Why

Beyond `oz` (broken-now, fixed by the companion inbox spec) and
`opencode` (added by the companion inbox spec), the user's machine
has two more authenticated CLI agents that *could* serve stage 2:
`codex` (OpenAI's Codex CLI) and `gemini` (Google's Gemini CLI).
Each has a clean JSON event stream we can parse, lives behind a
subprocess just like our existing CLI backends, and uses the
user's existing auth — no API key plumbing on our side.

Adding them is mechanically straightforward (subprocess +
NDJSON-parse + `parse_worker_id`, same shape as oz / opencode).
The reason this is *not* the priority path:

- **Codex default model: P50 ≈ 7s** (measured 2026-05-15, three
  runs). Slower than opencode by ~2x. The faster models
  (`gpt-5-mini`, etc.) require API-key auth that the user
  doesn't have wired in — they're on the ChatGPT-account auth
  flow, which restricts the available models.
- **Gemini `gemini-2.5-flash`: P50 ≈ 6s but flaky** — one of three
  runs hit a 30s timeout in our benchmark. `gemini-2.5-pro` and
  `gemini-3.1-pro` returned `ModelNotFoundError` on the user's
  account. The headless invocation also requires `--skip-trust`
  per directory, which the wizard would need to handle.
- Neither *beats* a fixed-oz (8-15s) by a margin that warrants
  the wizard complexity yet. The high-value alternative is
  `opencode` (~3s).

So they're real options for users who *do* live in those
ecosystems and prefer to stay there — worth designing for, but
not worth shipping until someone asks.

## What

Each candidate is a child spec that ships independently. None
exists yet; this document holds the design notes so picking up
any of them is a half-hour ramp rather than re-doing the
benchmark + auth investigation.

### Child A — codex backend (`CodexCliBackend`)

- [ ] Argv:
      `codex exec --json [-c model="X"] <prompt>`. The `--json`
      flag emits `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}`
      among other event types.
- [ ] Parse the stream: filter `type == "item.completed"` where
      `item.type == "agent_message"`, concat `item.text`, pass to
      `parse_worker_id`. No `agent_message` events surfaced →
      `Ok(None)`.
- [ ] Default model handling: **omit the `-c model=…` override**
      and let codex use its account-default. The user's
      ChatGPT-account auth restricts which models can be selected
      (verified: `gpt-5-mini` returns a 400). Leaving the default
      alone gives the broadest compatibility; advanced users can
      override in TOML.
- [ ] Default timeout: 30s (mirrors fixed oz). P50 ≈ 7s, P95
      unknown but the reasoning models (`o3-mini`) could plausibly
      take longer.
- [ ] Wizard branch: PATH-gated on `which::which("codex")`. Skip
      the model Select (model picker depends on the user's
      ChatGPT plan; we can't enumerate reliably). Free-text Input
      with a doc-link, or just skip and let codex default.

### Child B — gemini backend (`GeminiCliBackend`)

- [ ] Argv: `gemini --skip-trust -p <prompt> -m <model> -o json`.
      The `--skip-trust` flag is required for headless runs —
      gemini CLI otherwise refuses to execute outside an
      explicitly-trusted directory.
- [ ] Output shape: gemini's `-o json` emits a single top-level
      JSON object with a `response` field (the model reply) when
      it succeeds, or an `error` field with details when it
      fails. Parse with `serde_json`; on `response` present hand
      to `parse_worker_id`; on `error` present return `Err`
      (cascade swallows it and falls through).
- [ ] Default model: `gemini-2.5-flash`. Worked in 2/3 benchmark
      runs at ~6s; the only model accessible without a paid plan
      on the test account.
- [ ] Default timeout: 30s. The flakiness — one in three runs
      hit a server-side 30s wall — is a known concern; the
      cascade will fall through cleanly when it happens.
- [ ] Wizard branch: PATH-gated on `which::which("gemini")`.
      Free-text model Input (the catalog depends on the account
      / region; we can't reliably enumerate). Document
      `gemini-2.5-flash` as the suggested default.
- [ ] **Out of scope for the child:** auto-trusting the user's
      Jarvis runtime directory at install time. The
      `--skip-trust` flag at invocation time is enough; we don't
      need to mutate `~/.config/gemini-cli/trust.json`.

### Child C — future CLI agents

- [ ] Reserve the slot. Any new CLI agent that emits JSON event
      streams and exposes a subprocess prompt-reply contract
      slots in trivially. Examples that may surface:
      `ollama run --format json`, custom agent CLIs that
      ship with a `--ndjson` flag, etc.
- [ ] Promote when a real user asks.

## How

Implementation notes shared across children:

- After `opencode` lands (companion inbox spec), we'll have
  three CLI-agent backends following the same shape: subprocess
  + NDJSON or single-JSON parse + `parse_worker_id`. **That's
  the right time to extract a shared trait helper or function**
  for "filter events, concat texts, hand to parser". Don't do
  it before; premature abstraction.
- Each child should ship with at least: one captured-output
  fixture test (real JSON from the CLI), one
  `build_llm_stage` integration test, and an example block in
  `config/config.example.toml`.
- Per-backend auth quirks belong in the *child* spec body, not
  here. Above are the notes I gathered while benchmarking; they
  may have drifted by the time the child is built.

## Out of scope (umbrella-wide)

- Auto-discovery of *which* CLI agents are installed and
  recommending the fastest one. We tried this kind of "smart
  default" reasoning during 0014's design and consistently
  found it tripped over real users' edge cases. Explicit config
  wins.
- Per-call routing — picking a different backend for different
  prompt categories. Stage-2 design is "one configured backend,
  one classifier call per turn". Adding routing on top of
  classification is a different spec entirely.
- Cost / token-budget tracking across backends. Each CLI agent
  exposes its own usage data in its JSON stream (we saw it in
  oz's `request_usage.inference_cost` and opencode's
  `step_finish.cost`). Worth surfacing eventually but not the
  priority.

## Journal

- 2026-05-15: opened as umbrella. Drafted alongside the priority
  fix-oz-plus-opencode spec, after a benchmark across all four
  CLI agents the user had installed showed codex (P50 7s) and
  gemini-2.5-flash (P50 6s, flaky) as workable-but-not-winners
  vs. opencode (P50 3s). Captured the auth / model / output-format
  quirks here so picking up either child later doesn't require
  re-discovering them.
