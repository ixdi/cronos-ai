---
name: cronos-specification-generation
description: Write the OpenSpec change for a Cronos-AI task so a human can approve it and a separate agent can implement it without guessing. Use in the specification-generation step.
---

# Specification generation

You turn a task description into an OpenSpec change that a human approves before any code is written.
The Cronos-AI contract at the end of your prompt names the change, the result file, and the rules you must follow; it always wins over this skill.

## Before writing

1. Read the task title, description, and any revision note from the latest rejected approval.
   A revision note is the human's direct feedback: address every point in it.
2. Read `openspec/specs/` to learn the current behavior of the capabilities the task touches.
3. Read the code those capabilities live in, enough to know what exists and what must change.
   Do not plan changes to code you have not looked at.
4. If the change directory already exists, update it instead of starting over.

## What to write

- `proposal.md`: **Why** (the problem, in the user's terms), **What Changes** (observable behavior, as a short list), **Capabilities** (new or modified), and **Impact** (affected areas, dependencies, migrations).
- `specs/<capability>/spec.md`: delta requirements under `## ADDED Requirements`, `## MODIFIED Requirements`, or `## REMOVED Requirements`.
  Every requirement uses SHALL and has at least one `#### Scenario:` with **WHEN** and **THEN** lines.
  A MODIFIED requirement repeats the full requirement text, not only the changed sentence.
- `design.md`, only when there are real decisions to record: the options considered, the choice, and why.
- `tasks.md`: small, ordered, checkable tasks (`- [ ] 1.1 ...`), each naming the tests that prove it.
  Include tests for every scenario, and end with running the project's checks.

## Quality bar

- Scope the change to the task; list related improvements as out of scope instead of adding them.
- Describe behavior, not implementation details, in specs; keep implementation choices in the design.
- Make every scenario testable: concrete inputs, concrete observable outcomes.
- Cover error and edge cases the task implies, such as invalid input, empty states, and concurrent use.

## Finish

1. Run `openspec validate <change> --strict` and fix every problem it reports.
2. Commit only the change directory with a Conventional Commit message such as `docs(spec): propose <change>`.
3. Confirm `git status` is clean, then write the result file the contract describes.
