---
id:                        # 0001-style, four digits. Leave null in inbox/.
title: Short noun phrase
status: inbox              # inbox | active | shipped | rejected
owner: unassigned          # github username, "unassigned", or "agent"
created: 2026-05-13        # ISO date
shipped:                   # optional, set when moved to shipped/
verifying:                 # each bullet should be runnable / observable
  - tests/<file>.rs::<test>
related:                   # other specs that touch this one (id list)
---

# {Replace with the title}

## Why

One to three paragraphs. Describe the problem this spec exists to solve.
Focus on the **user-visible** or **maintainability-visible** pain — not
the implementation. A reader should be able to decide whether this spec
deserves attention without ever touching code.

## What

Concrete, verifiable acceptance criteria. Each bullet must be something
a stranger can verify yes/no without asking you. Use checkboxes:

- [ ] Acceptance criterion 1
- [ ] Acceptance criterion 2
- [ ] Acceptance criterion 3

If you can't make a criterion testable, the design isn't done yet.
Promote to active **only** after at least three concrete bullets exist.

## How

Optional. Use this section for implementation sketches and non-obvious
design decisions. Leave it empty if the implementation is "do the
obvious thing".

Some prompts that often want answers:

- Which modules / files are affected?
- What's the migration path for existing users (if breaking)?
- Are there hard tradeoffs (latency vs accuracy, etc.) that we chose
  consciously?

## Journal

Append-only. Each entry is a dated bullet point describing a decision
taken while working. Use this for "I chose A over B because…" and
"discovered C; pivoted plan". A reader picking the work up after a
break should be able to reconstruct your thinking.

- 2026-MM-DD: opened.
