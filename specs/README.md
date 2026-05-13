# Specs

Jarvis uses lightweight spec-driven development. A **spec** is a short
markdown file that captures the *intent* of a change before (or alongside)
the code. Specs are the canonical source of truth for "what are we
building and why"; the code is the source of truth for "how it works
right now".

The two motivations:

1. **Voice-driven workflow.** Long-term goal is to use Jarvis itself to
   manage Jarvis — say "Mutombo, open a spec for ..." and the agent
   creates the file, refines acceptance criteria across turns, and
   promotes it when the design is solid.
2. **Contributor onboarding.** A new contributor reading `specs/shipped/`
   learns the project's design history without spelunking through
   commits.

## Lifecycle

```
specs/private/   ─ ignored by git, your personal scratch
specs/inbox/     ─ ideas worth sharing but not yet committed to
specs/active/    ─ accepted, being implemented now
specs/shipped/   ─ done; kept as living documentation
specs/rejected/  ─ decided against, with the reason logged
```

Day-to-day flow:

1. **Inbox.** A new idea lands in `specs/inbox/` as
   `YYYY-MM-DD-short-slug.md`. No ID assigned yet. Body can be rough —
   just enough that another human (or the agent) understands the intent.
   Inbox items are **shared but informal**; nobody has committed to
   building them.

2. **Promotion.** When the design is solid enough that we're ready to
   build, the file is renumbered with the next sequential ID and moved
   to `specs/active/<NNNN>-slug.md`. Acceptance criteria must be
   testable at this point.

3. **Implementation.** Code goes against an active spec. The spec's
   `## Journal` section is updated with non-obvious decisions made
   during the work (so a future reader knows *why*, not just *what*).

4. **Shipping.** When every checkbox in `## What` passes, the file moves
   to `specs/shipped/`. From that point the file is treated as
   read-only history — corrections happen via a new spec that references
   the old one, not via edits.

5. **Rejection.** If we decide *not* to do it, the file moves to
   `specs/rejected/` with a short `## Reason rejected` section appended
   to the body. The history is valuable even when the answer is no.

## File format

Filename:

- `specs/inbox/`: `YYYY-MM-DD-short-slug.md`. No ID, date prefix
  preserves chronological order on disk.
- everywhere else: `NNNN-short-slug.md`. Four-digit ID, kebab-case slug,
  no date (the frontmatter has it).

Frontmatter:

```yaml
---
id: 0014                # null in inbox/; assigned at promotion to active
title: Pluggable wake backends
status: active          # inbox | active | shipped | rejected
owner: tadeo            # who's driving this; can be "unassigned"
created: 2026-05-13
shipped:                # optional; set when moved to shipped/
verifying:              # acceptance — how do we know it's done?
  - tests/wake_integration.rs::pluggable_backend_smoke
  - cargo run -- test-wake --threshold 0.02
related:                # other specs that touch this one
  - 0011                # config schema versioning
---
```

Body — these four sections, in order, each starting with an H2:

- **`## Why`** — motivation. 1–3 paragraphs. The problem we're solving,
  not the implementation. Should still make sense if read in isolation.
- **`## What`** — acceptance criteria as a bullet list. Each bullet must
  be testable (someone reads it later and can say yes/no). Use checkbox
  syntax: `- [ ]` for pending, `- [x]` for met.
- **`## How`** — implementation sketch. Optional. Use this for non-obvious
  design decisions that future contributors need to know. Leave empty
  when the implementation is "do the obvious thing".
- **`## Journal`** — append-only log of decisions taken while
  implementing. Dated bullet points. Future you (or the agent) reads
  this when picking up the work after a gap.

Keep specs **short** — under 200 lines is the target. If your spec is
longer, it probably wants to be two specs.

## Promotion rules

A spec moves from `inbox/` to `active/` when:

- [ ] It has at least three concrete bullets in `## What` that pass the
      "could a stranger verify this?" test.
- [ ] An owner is named in frontmatter.
- [ ] No fundamental disagreement is unresolved in the journal.

A spec moves from `active/` to `shipped/` when **every** checkbox in
`## What` is `[x]` and the verifying steps in frontmatter pass.

## Shared vs private

`specs/private/` is gitignored. Use it for half-baked ideas you don't
want in the project's history. Anything you'd be embarrassed for a
stranger to see at 3am goes there.

`specs/inbox/` is shared. Don't be afraid of putting rough ideas in
shared inbox — that's literally what it's for. Half-formed thinking in
the open is more valuable than polished thinking in private.

## Voice workflow (planned, step 3)

Once the voice-driven spec commands land, the flow will be:

```
"Mutombo, create a spec for streaming TTS"
  → agent creates specs/inbox/YYYY-MM-DD-streaming-tts.md
    pre-populated with empty sections

"Mutombo, add as acceptance criteria that latency under 200ms"
  → agent appends to ## What

"Mutombo, promote the streaming TTS spec"
  → agent moves to specs/active/NNNN-streaming-tts.md
    and assigns the next sequential ID
```

Until that ships, all of this happens manually with your editor.

## Numbering

IDs are sequential integers (`0001`, `0002`, ...) and never reused.
When promoting a spec from `inbox/` to `active/`, run:

```sh
ls specs/active specs/shipped specs/rejected | grep -oE '^[0-9]{4}' | sort -n | tail -1
```

to find the highest used ID, then use the next one. Yes, this races if
two contributors promote simultaneously — that's a future problem.

## Why this format and not RFCs / ADRs / GitHub Spec-Kit?

Honest answer: those are heavier than what we need.

- **RFCs** (Rust/Python style) presume formal review by a steering
  committee. Overkill for one or two maintainers.
- **ADRs** (architecture decision records) capture *decisions* but not
  the work-tracking dimension we want.
- **GitHub Spec-Kit** is closest in spirit but assumes a particular
  agent runtime. We want plain markdown the agent can read but humans
  can edit comfortably.

This format is deliberately small. If we outgrow it, we'll know — and
we'll write a spec to migrate.
