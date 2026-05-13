---
id: 0001
title: Pluggable AI agents
status: shipped
owner: tadeo
created: 2026-05-12
shipped: 2026-05-13
verifying:
  - cargo run -- test-agent "ping"
  - cargo test --test cli
related:
  - 0003  # conversation sessions (changed the trait signature)
---

# Pluggable AI agents

## Why

The point of Jarvis is to wire your voice to *any* AI agent. Hard-coding
a single backend would force users to pick our blessed model and would
let one upstream API outage take the entire assistant down. We want the
agent to be a config-driven choice — claude, openai, gemini, a Warp
session, or any shell command — so users can swap without recompiling
and so support for new backends is additive, not a rewrite.

A second motivation: this is the natural extension point for "Jarvis
that writes code with Claude Code, and answers small questions with a
local llama". Once the trait surface is clean, alternative agents are
~150 LOC each.

## What

- [x] An `Agent` trait with a `respond(prompt, history) -> String` method
  lives in `src/agents/mod.rs`.
- [x] Built-in backends: Claude Code (`claude` CLI), OpenAI Chat
  Completions, Google Gemini, Warp `oz`, and a generic `shell` agent
  that pipes prompts to stdin of any user-supplied command.
- [x] Selection by config string: `[agent] name = "claude"` (aliases:
  `claude-code`, `chatgpt`, `google`, `oz`).
- [x] Each backend in its own file under `src/agents/<name>.rs`. The
  registry in `mod.rs` is the only place that knows about all of them.
- [x] `jarvis doctor` reports per-agent install / auth status.
- [x] `jarvis test-agent "<prompt>"` exercises the configured agent
  end-to-end with no audio.
- [x] API-key precedence: explicit `api_key` in config beats environment
  variable; environment is the recommended path.

## How

The trait is intentionally small:

```rust
pub trait Agent {
    fn name(&self) -> &'static str;
    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String>;
}
```

CLI-based backends (Claude, Warp shell, generic shell) embed the prompt
on stdin and read the reply from stdout. HTTP-based backends (OpenAI,
Gemini) use `ureq`. We deliberately do *not* use a streaming async
runtime — synchronous request/response is sufficient and keeps the
dependency tree small.

Plugin discovery: the registry in `agents/mod.rs` matches `cfg.agent.name`
against a static `match` block. Custom agents are added by writing a new
module and one match arm — there is no dynamic plugin loader.

## Journal

- 2026-05-12: rejected hard-binding to a single SDK (anthropic-sdk-rs)
  in favour of subprocessing the CLI. Reasoning: it keeps Jarvis's
  binary tiny and lets users keep their Claude Code experience as-is
  (tool use, MCP servers, sandboxed shell).
- 2026-05-12: rejected a dynamic plugin loader (.so / dlopen). Adds
  build complexity; the `shell` agent already covers the "I want any
  arbitrary tool" case via subprocess.
- 2026-05-13: added Warp `oz` agent. Auto-detects `oz`, `oz-preview`,
  and the deprecated `warp-cli` (warns on the latter).
- 2026-05-13: changed trait signature to take `&[Turn]` history (see
  spec 0003 — conversation sessions). All existing backends updated;
  one-shot callers like `test-agent` pass an empty slice.
