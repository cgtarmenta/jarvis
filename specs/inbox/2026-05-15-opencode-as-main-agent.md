---
id:
title: opencode as a main agent (stage-3 worker)
status: inbox
owner: unassigned
created: 2026-05-15
shipped:
verifying:
related:
  - shipped/0001-pluggable-ai-agents.md
  - shipped/0008-orchestrator-c-worker-manifests-and-auto.md
  - inbox/2026-05-15-stage-2-fix-oz-add-opencode.md
---

# opencode as a main agent (stage-3 worker)

## Why

Today the `[agent]` block in the wizard offers five choices:
`claude`, `openai`, `gemini`, `warp` (the oz CLI), and `shell`.
This is the **stage-3 default worker** — the agent that takes
every voice turn the deterministic intent matchers and (optional)
stage-2 classifier didn't claim. It's where the heavy thinking
happens: "refactor X", "explain this file", multi-turn coding
sessions.

The companion inbox spec
(`2026-05-15-stage-2-fix-oz-add-opencode.md`) adds `opencode` as a
**stage-2 classifier backend** — fast, free, ~3s/turn. That's a
different surface: stage 2 is a one-shot routing decision per
turn, not a multi-turn conversation owner.

But `opencode` is *also* a credible **main agent** in its own
right:

- The user already has it installed and authenticated.
- Its free-tier models (`opencode/qwen3.6-plus-free`,
  `opencode/big-pickle`, `opencode/deepseek-v4-flash-free`) handle
  the lightweight chat use cases at no cost.
- Its paid-tier models (mistral/devstral-medium-latest,
  qwen3.6-plus, anthropic claude variants via opencode's own
  routing) handle heavy coding work.
- Multi-turn session continuity is first-class — `opencode run -c`
  or `--session <id>` resumes a session, same shape as
  `claude --resume <uuid>` that `ClaudeAgent` already uses.
- JSON event stream (`--format json`) means no fragile output
  parsing, unlike some of our existing agent wrappers.

Adding `opencode` as a sixth main-agent option gives users a
free, fast, locally-installed alternative to `claude` for the
"main thinking" worker, without forcing them through the `shell`
agent's no-batteries-included generic path.

## What

- [ ] New `src/agents/opencode.rs` exposing `OpencodeAgent`
      implementing the `Agent` trait
      (`src/agents/mod.rs::Agent`). Methods:
  - `name() -> "opencode"`
  - `respond(prompt, history) -> Result<String>` invokes
    `opencode run --format json [-c | --session <id>] -m <model> <prompt>`,
    streams the NDJSON, extracts `type == "text"` events,
    concatenates `part.text`, returns the joined string.
  - `current_session_id() -> Option<String>` returns the opencode
    session id captured from the previous `step_start` event
    (which carries `sessionID`), so spec 0009's
    `dispatched_to` / `active_workers` schema gets populated
    correctly.
- [ ] Constructor `OpencodeAgent::from_options(opts: toml::Table)`
      with these recognised keys (all optional, sensible defaults):
  - `binary` → command to invoke; defaults to `opencode`.
  - `model` → the `provider/model` id; defaults to
    `opencode/qwen3.6-plus-free` (free + 3s/turn measured
    2026-05-15).
  - `agent` → the opencode-side agent profile, passed to
    `opencode run --agent <name>`. Defaults to opencode's
    built-in "build" or "general" — TBD during implementation;
    open question to resolve before promoting.
  - `cwd` → working directory for the subprocess. Defaults to
    the daemon's cwd, mirroring `ClaudeAgent`'s behaviour.
- [ ] Wire `OpencodeAgent` into `agents::build`. Match
      `"opencode"` in the dispatch.
- [ ] Wizard (`src/setup/mod.rs::configure_agent`): add `opencode`
      to the agents Select. Skip the api_key prompt
      (`opencode` handles its own auth). Optionally check
      `opencode auth list` (or whatever opencode exposes for
      auth status) and surface a `→ run \`opencode auth login\``
      hint if no providers are configured. Same envelope as the
      `configure_warp_auth` flow added in spec 0014.
- [ ] Wizard collects the `model` field via the same multi-column
      table + tab-completion machinery built for oz in spec 0014.
      Source from `opencode models`; default to
      `opencode/qwen3.6-plus-free`.
- [ ] Session continuity:
  - First turn (no prior session id): spawn with neither
    `--continue` nor `--session`; capture the session id from
    the first `step_start` event in the NDJSON stream and store
    in `cfg.agent.options["session_id"]` (in-process; not
    persisted to TOML — same pattern as `ClaudeAgent`).
  - Subsequent turns: spawn with `--session <stored-id>`.
- [ ] Existing `[session].agent_session` field in
      `JarvisConfig::session` already covers persisting agent
      session ids across daemon restarts — `OpencodeAgent`
      should plug into the same wiring `ClaudeAgent` uses
      (resolved via spec 0003 / 0005 / 0006). No new config
      surface required.
- [ ] Tests (`src/agents/opencode.rs::tests`):
  - Fixture stream (`step_start` + `text` + `step_finish`):
    `respond` returns the concatenated `text` values.
  - Fixture stream with multiple `text` events: concatenated in
    order.
  - Fixture stream missing any `text` event: returns an
    informative error (or empty string — match the existing
    agents' contract; check what `ClaudeAgent` does for empty
    replies).
  - `current_session_id` returns the id from `step_start` and
    persists across `respond` calls.
- [ ] Config docs: add an `[agent]` example block for opencode in
      `config/config.example.toml` showing the optional
      `model` / `agent` keys.

## How

Implementation sketch:

- **Mirror `WarpAgent` / `ClaudeAgent` first, look at differences
  later.** Both are existing subprocess-based agents with their
  own quirks; `OpencodeAgent` shares more with `ClaudeAgent` than
  any of the HTTP-based agents because of the session-id
  continuity model.
- **Re-use parsing.** If the companion stage-2 spec lands first,
  it'll have already implemented an `opencode_json_stream` parser
  (filter `type == "text"`, concat `part.text`). Lift that into a
  shared utility in `src/agents/opencode_utils.rs` or
  `src/utils/opencode.rs` so the agent and the stage-2 backend
  share the parser. **Don't duplicate.**
- **Streaming TTS.** Out of scope for v1, but worth noting in the
  journal: opencode's NDJSON stream means we *could* feed TTS
  with partial replies as they arrive instead of waiting for the
  full response. Other agents are blocking. Park as a future
  optimisation note in the agent module's docstring.
- **Agent profile.** opencode supports per-call `--agent <name>`
  to switch system-prompt / tool-set. Resolving the right
  default during implementation needs a small `opencode agent
  list` probe — defer to implementation phase. If unclear, omit
  the flag and let opencode use its own default agent.

## Out of scope

- Migrating existing users from `claude` to `opencode`. Default
  stays `claude`; users opt into `opencode` via wizard or TOML.
- Streaming TTS for partial replies. Future.
- Using one opencode session simultaneously for stage-2 and
  stage-3 calls. The two surfaces stay independent — they
  happen to share a binary but the auth/state model is per
  invocation.
- Auto-detecting "best" main agent on first run. Per the
  feedback pattern this repo already follows, explicit config
  wins over heuristics.

## Journal

- 2026-05-15: opened. User flagged opencode as a credible main-
  agent candidate after the stage-2 benchmark showed it fastest
  and free. Distinct from the stage-2 inbox spec because the
  design surface differs: stage 2 is one-shot routing, stage 3
  is multi-turn conversation with session continuity and project
  context. Both share the opencode binary and JSON event stream,
  so the parser is reusable across the two — noted in the
  How section.
