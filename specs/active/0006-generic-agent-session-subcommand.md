---
id: 0006
title: Generic agent session subcommand
status: active
owner: tadeo
created: 2026-05-13
shipped:
verifying:
  - cargo run -- agent --help
  - cargo run -- agent status   # under each agent type
  - cargo test specs::tests       # CLI smoke tests
related:
  - 0001
  - 0005
id: 
shipped: 
---

# Generic agent session subcommand

## Why

Spec 0005 shipped `jarvis claude {sessions,attach,detach,status}` for
managing Claude Code sessions. The functionality is correct but the
naming is wrong: the agent is already chosen by `[agent].name` in
config, so hard-coding `claude` in the subcommand path is redundant
and locks the surface to one backend. When Warp's `oz` session
support lands (spec TBD), we'd either need a separate `jarvis warp
attach` or break the user-facing CLI again.

User observation that triggered this:

> *"veo que el comando para manejar las sesiones es `jarvis claude`,
> pero eso asume que el agente es claude — debería ser algo más
> agnóstico no?"*

The right shape is `jarvis agent <verb>` and per-agent dispatch
inside the handler — same pattern as `[agent].name` in config and the
`agents::build` registry.

Status of session support per agent (snapshot at writing):

| Agent       | Has external sessions | Implemented in Jarvis |
|-------------|-----------------------|-----------------------|
| claude      | yes (`--resume`)      | yes (spec 0005)       |
| warp / oz   | yes (`--session-id`)  | no (roadmap)          |
| openai      | stateless API         | n/a — `jarvis session` |
| gemini      | stateless API         | n/a — `jarvis session` |
| shell       | depends on command    | n/a                   |

For agents without an external session concept, the subcommand
should not just error — it should explain the situation and point
at `jarvis session` (the Jarvis-side conversation history).

## What

- [ ] `jarvis claude` is removed as a CLI subcommand. The functionality
      moves under `jarvis agent`.
- [ ] `jarvis agent sessions [--cwd PATH] [--limit N]` works when
      `[agent].name = "claude" | "claude-code"` and behaves
      identically to the old `jarvis claude sessions`.
- [ ] `jarvis agent attach <uuid|--latest [--cwd PATH]>` behaves
      identically to the old `jarvis claude attach`.
- [ ] `jarvis agent detach` and `jarvis agent status` behave
      identically to the old equivalents.
- [ ] When the configured agent has no external session concept
      (openai, gemini, shell), `jarvis agent <verb>` prints a clear
      message naming the agent and pointing at `jarvis session` for
      Jarvis-side conversation history. Exit code 0 — informational,
      not an error.
- [ ] When the configured agent is `warp` / `oz`, `jarvis agent`
      verbs print "not yet implemented for warp" with a pointer to
      the roadmap. Exit code 0.
- [ ] `jarvis doctor` line previously labelled `claude attach`
      becomes `agent attach`, still only populated for the claude
      agent (other agents are stateless or roadmap).
- [ ] README's "Notify on long-running tasks" + any other mentions of
      `jarvis claude` are updated.
- [ ] CHANGELOG entry under `## [Unreleased]` documenting the rename.

## How

The plumbing in `agents::claude_attach` stays exactly as is — that
module is correctly named because it's Claude-specific
implementation, not the user-facing surface.

The CLI level rewires: drop `Cmd::Claude { cmd: ClaudeCmd }`, add
`Cmd::Agent { cmd: AgentCmd }`. The new `cmd_agent` handler
dispatches on `cfg.agent.name`:

```rust
match cfg.agent.name.as_str() {
    "claude" | "claude-code" => handle_claude_session_cmd(cmd, cfg),
    "warp" | "oz"            => print_warp_roadmap(),
    "openai" | "chatgpt"
    | "gemini" | "google"
    | "shell"                => print_stateless_explanation(name),
    other                    => print_unknown(other),
}
```

We *don't* introduce a `SessionsCapable` trait yet — that's the
right abstraction once a second agent (warp) actually implements
sessions. YAGNI for v1. A `match` against `cfg.agent.name` is fine
for one real impl.

`jarvis claude` is removed cleanly rather than aliased to `jarvis
agent`. Nothing in the wild depends on it yet (shipped 30 min ago
on master) and aliases add maintenance forever.

Spec 0005 stays in `specs/shipped/` unmodified — that's our
immutability rule. This spec references it from `related:`.

## Journal

- 2026-05-13: promoted to active.

- 2026-05-13: opened during user-feedback iteration.
- 2026-05-13: sharpened. Confirmed YAGNI on the trait — `match` over
  `cfg.agent.name` is fine until warp sessions land. Reaffirmed the
  immutability rule for spec 0005 (it stays in `shipped/`; this
  spec supersedes its CLI surface). Ready to promote.
