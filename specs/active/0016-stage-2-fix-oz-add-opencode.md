---
id: 0016
title: Stage-2 backend — fix oz parsing/timeout + add opencode
status: active
owner: tadeo
created: 2026-05-15
shipped:
verifying:
  - src/dispatcher/llm/oz_cli.rs tests
  - src/dispatcher/llm/opencode_cli.rs tests
  - tests/stage_2_backends.rs
related:
  - shipped/0013-orchestrator-b-llm-dispatcher-backends.md
  - shipped/0014-setup-wizard-dispatcher-fallback.md
  - inbox/2026-05-15-stage-2-additional-backends.md
  - inbox/2026-05-15-opencode-as-main-agent.md
---

# Stage-2 backend — fix oz parsing/timeout + add opencode

## Why

Manual end-to-end testing of spec 0014's wizard against the user's
real setup (`backend = "oz"`, `model = "auto-open"`) surfaced two
real bugs in shipped spec 0013/B-3's `OzCliBackend`, and one big
gap that 0014 made obvious:

1. **`OzCliBackend::DEFAULT_TIMEOUT_SECS = 5` is wildly too short.**
   Empirical measurement on 2026-05-15 across oz's catalog: P50
   classifier latency ≈ 8s, P95 ≈ 15s. Even the fastest fast-tier
   models (`auto-efficient`, `claude-4-5-haiku`, `kimi-k25-fireworks`)
   land at 8s+. Every voice turn currently logs
   `WARN oz classifier timed out after 5s` and falls through to
   stage 3.

2. **`parse_worker_id` reads `reply.lines().next()`.** But the first
   line of `oz agent run` stdout is the run handle
   `Run ID: 019e…`, not the model reply. The model reply lands
   later in the stream. So `parse_worker_id` extracts the token
   `Run` (or the UUID), no worker matches, returns `None`, cascade
   falls through. **Stage 2 with `backend = "oz"` has never worked
   in practice**, even when calls succeed within the timeout.

3. **No "fast, free, zero-setup" stage-2 path exists.** `openai_compat`
   is fast but requires the user to run their own endpoint
   (Triton / Ollama / vLLM / Groq). The other CLI backend `oz`
   even when fixed pays 8-15s/turn — borderline unusable for voice.
   A benchmark on 2026-05-15 of every CLI agent the user had
   installed (oz, codex, gemini, opencode) showed
   **opencode's free models reply in 2.7-3.6s with correct,
   parseable JSON** — competitive with cloud endpoints and the
   fastest CLI-agent classifier by a wide margin. Adding it as a
   third stage-2 backend gives users without their own endpoint
   a default that actually works for voice.

Combined, this slice restores `oz` to working order and adds
`opencode` as the new recommended "easy" default — closing the
gap between "stage 2 is wired in" and "stage 2 is actually useful".

## What

### Fix OzCliBackend (touches shipped 0013/B-3)

- [ ] Invoke `oz agent run` with `--output-format json` in
      `OzCliBackend::classify`. The flag was missing in B-3, which
      is why the wrapper saw the human-readable handle preamble
      instead of structured events.
- [ ] Parse the resulting NDJSON stream. Filter events by
      `type == "agent"` (the model's user-facing reply, distinct
      from `agent_reasoning` chain-of-thought and `system`
      run/conversation handles). Concatenate the `text` fields in
      order, then hand the concatenation to the existing
      `parse_worker_id`.
- [ ] If the stream contains zero `agent` events before subprocess
      exit (only `agent_reasoning` or `system`, e.g. a reasoning
      model that exhausted its budget thinking), return `Ok(None)`.
      The cascade falls through to stage 3 — same envelope as
      every other "decline" path. **Do not** error out.
- [ ] Default timeout: `DEFAULT_TIMEOUT_SECS = 5` → `30`. P95
      across realistic models is ≈15s; 30s is 2x P95 headroom
      without locking the cascade out for unbounded time. Users
      who want tighter can override in TOML.
- [ ] Tests (`src/dispatcher/llm/oz_cli.rs`):
  - Fixture stream with `run_started` + `conversation_started` +
    `agent_reasoning` + `agent` events → extracted text matches
    the `agent` event(s) only.
  - Fixture stream with multiple `agent` events → concatenated in
    order.
  - Fixture stream with only `agent_reasoning` → `Ok(None)`.
  - Empty stream → `Ok(None)`.
  - Malformed JSON line ignored, valid lines still parsed.

### Add OpencodeCliBackend (new file)

- [ ] New module `src/dispatcher/llm/opencode_cli.rs` mirroring the
      shape of `oz_cli.rs`. Public type `OpencodeCliBackend`
      implementing `LlmBackend`. Constructor takes a model id
      (`provider/model` shape per opencode's convention).
- [ ] Argv: `opencode run --format json -m <provider/model> <prompt>`.
      No api_key plumbing — `opencode` handles auth via its own
      login store, same convention as `oz`.
- [ ] Parse NDJSON. Filter events by `type == "text"` and read
      `part.text` (the actual model reply tokens). Concatenate;
      hand to `parse_worker_id`. Other event types
      (`step_start`, `step_finish`) are ignored.
- [ ] If no `text` events surfaced before exit: `Ok(None)`.
- [ ] Default timeout: 15s. Empirical P50≈3s, P95<5s across
      `*-free` models; 15s gives 3x P95 headroom.
- [ ] Default model exposed to wizard-skip / config-default cases:
      `opencode/qwen3.6-plus-free` (measured median 3.06s with
      correct replies). Override via wizard or TOML.
- [ ] Cascade integration (`src/dispatcher/llm/cascade.rs`):
      `build_llm_stage` accepts `backend = "opencode"` and
      constructs an `OpencodeCliBackend`. Same shape as the
      existing oz / openai_compat branches.
- [ ] Tests (`src/dispatcher/llm/opencode_cli.rs`):
  - Real-captured fixture: `step_start` → `text` (with `part.text`
    = `time\n`) → `step_finish` → extracted text = `"time\n"`.
  - Fixture with multiple `text` events → concatenated.
  - Fixture with only `step_start` + `step_finish` → `Ok(None)`.
  - `build_llm_stage` accepts a minimal opencode block
    (`backend = "opencode"`, `model = "opencode/qwen3.6-plus-free"`)
    and rejects unknown fields with `deny_unknown_fields`-style
    error messages.

### Wizard surface (touches shipped 0014)

- [ ] In `src/setup/dispatcher.rs::configure_dispatcher_fallback`,
      add `opencode` as a third backend choice in the Select
      gated on `which::which("opencode").is_ok()`. Label suggestion:
      `opencode (free models, ~3s)`.
- [ ] New `collect_opencode(theme)` mirroring `collect_oz`:
      - Try to fetch the model list via `opencode models` (newline-
        delimited `provider/model`). On success, present a
        Select-with-completion using the same multi-column table
        + `ModelCompletion` machinery built for oz in 0014.
        Default the Select to `opencode/qwen3.6-plus-free` (or
        the first match starting with `opencode/` for forward
        compatibility if the model id changes).
      - On fetch failure: fall through to a free-text Input with
        the soft warning pattern.
      - Build the `[dispatcher.fallback]` table with
        `backend = "opencode"` + `model = <picked>`. Optionally
        prompt for `timeout_secs` (default 15) — parity with the
        oz branch (which this spec adds; see below).
- [ ] Parity touch-up on `collect_oz`: expose `timeout_secs` as
      an optional Input (default 30 after the bump) — the spec
      0014 originally skipped this because "defaults are right";
      empirical data on 2026-05-15 says they weren't, so the
      wizard now lets the user override.
- [ ] Live probe envelope for the new backend matches oz /
      openai_compat: one `classify` call against a one-worker
      fixture, surfaces `✓ classifier reachable` or
      `⚠ classifier didn't respond — saving config anyway`. Never
      blocks save.

### Config docs

- [ ] Add an `opencode` block to `config/config.example.toml`
      alongside the existing `oz` and `openai_compat` examples.
      Note the free-tier models with a one-line "stage 2 with
      opencode adds ~3s/turn" perf hint.
- [ ] Update the existing `oz` example block to note the new
      30s default timeout and a one-line "stage 2 with oz adds
      ~8-15s/turn" perf hint, so the user picks the right backend
      for their UX target.

### Integration tests

- [ ] Round-trip a wizard-shaped `backend = "opencode"` table
      through `config::load` → assert equivalent, then through
      `dispatcher::llm::build_llm_stage` → assert it builds.
- [ ] Round-trip the same with `backend = "oz"` after the
      timeout bump — confirms re-shipping doesn't break the v1
      shape.

## How

Implementation sketch:

- **Shared shape.** Both `oz_cli.rs` and the new `opencode_cli.rs`
  follow the same pattern: spawn subprocess, stream NDJSON, filter
  events, concatenate texts, parse worker id, surface error /
  timeout / decline. After the second backend lands, decide
  whether to extract a `parse_classifier_event_stream(filter_fn)`
  helper — but copy-paste-with-edits is fine for v1; premature
  abstraction adds debt without saving work.
- **NDJSON streaming vs collect-then-parse.** v1 reads the
  subprocess's full stdout to a String and then iterates lines.
  Memory cost is bounded (one classifier reply ≈ a few KB).
  True streaming (parse-as-you-go and short-circuit on first
  `agent` event) is an optimisation we can layer in later if the
  watchdog timing makes it worth it.
- **Empty-stream handling.** `Ok(None)` is the documented decline
  signal in the `LlmBackend` trait (per 0013/B-1). The cascade
  adapter (`LlmDispatcher::dispatch`) already treats `None` as
  "fall through to stage 3" and never lets the user's voice turn
  fail because of stage 2. We rely on that.
- **Opencode model default.** `opencode/qwen3.6-plus-free` chosen
  over `opencode/deepseek-v4-flash-free` because both measured
  3-4s but qwen is the more general-purpose instruct model and is
  less likely to over-think a routing prompt. Worth re-measuring
  before commit if the catalog has shifted.
- **Wizard model Select.** `opencode models` output is plain
  newline-delimited `provider/model` (verified 2026-05-15). The
  multi-column table + tab-completion we built for oz in 0014's
  Slice 7 ports over with minimal changes — the only new piece is
  filtering / preferring the `opencode/*-free` group at the top of
  the table so they're easy to spot.

Empirical measurements that drove the defaults are summarised
inline in the **Why** section; the bench scripts live in
`/tmp/oz_bench2.sh` and `/tmp/agents_bench.sh` for replay if
someone wants to re-measure before promotion.

## Out of scope

- **Codex / Gemini backends** — different inbox spec
  (`2026-05-15-stage-2-additional-backends.md`). Latency profiles
  (codex P50 ≈ 7s, gemini-2.5-flash ≈ 6s but flaky) aren't bad,
  but they don't *beat* a fixed-oz, so the urgency is lower.
- **A shared trait helper** for "subprocess + NDJSON + filter
  events + concat texts." Tempting but premature; do it after
  the second backend lands.
- **Auto-selecting the fastest available backend** at daemon
  startup. The user configures explicitly; we don't second-guess.
- **Streaming stage-2 results** rather than blocking the cascade
  on a complete `classify` reply. Real win would be for
  reasoning-heavy backends only and complicates the cascade
  contract a lot. Not now.

## Journal

- 2026-05-15: promoted to `active/` as **0016**. No design
  changes from the inbox draft — the bug analysis, benchmark
  data, and design sketch were ready as-shipped. `verifying:`
  seeded with the expected test surfaces (oz parser tests,
  new opencode_cli unit tests, integration tests for both
  backends through the wizard round-trip). Owner: tadeo per
  the convention 0015 set when promoting via voice.

- 2026-05-15: opened. Drafted after manual test of 0014 against
  the user's `backend = "oz"` config surfaced the two oz bugs,
  and after a follow-on benchmark across oz/codex/gemini/opencode
  CLI agents identified `opencode` as the fastest, correct, free
  CLI-agent classifier on the user's box. Scope decided as B in
  a triage exchange: fix oz (necessary anyway) + add opencode
  (the high-value path). Codex/Gemini deferred to an umbrella
  inbox spec for non-priority work.
