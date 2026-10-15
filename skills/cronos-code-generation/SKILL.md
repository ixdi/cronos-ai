---
name: cronos-code-generation
description: Implement an approved OpenSpec change for a Cronos-AI task with tests first, small commits, and a clean worktree. Use in the code-generation step, including revisions after review, verification, check, or merge findings.
---

# Code generation

You are the only writer in this step.
You implement the approved OpenSpec change, or fix the findings that sent the task back to you.
The Cronos-AI contract at the end of your prompt names the change, the result file, and the rules you must follow; it always wins over this skill.

## Start from the facts

1. Read the approved change: `proposal.md`, `design.md` if present, the spec deltas, and `tasks.md`.
   If the change was already archived, read it under `openspec/changes/archive/` and the synced specs in `openspec/specs/`.
2. On a revision, read the findings in your prompt first.
   Each one is a concrete problem to fix, not a suggestion to weigh.
3. Read the code you will change and the tests around it.
   Match the surrounding style, naming, error handling, and comment density.

## Work in small, tested steps

For each task in `tasks.md`:

1. Write or extend a test that fails for the missing behavior or the reported bug, and run it to see it fail.
2. Make the smallest change that makes it pass.
3. Run the related test suite, then mark the task `[x]` in `tasks.md`.
4. Commit with a Conventional Commit message that explains why, such as `feat(cli): ...` or `fix(store): ...`.

Cover every spec scenario with a test.
Prefer an end-to-end test that drives the program the way a user would when the behavior is user-visible.

## Keep the branch healthy

- Run the project's full test suite and linters before you finish; the project check command runs after verification and sends failures back to you.
- Do not weaken, skip, or delete tests to make them pass.
  If a test is wrong, fix it and explain why in the commit message.
- Do not leave generated files, logs, or build output in the worktree; add them to `.gitignore` if the build produces them.
- Do not add dependencies the design did not call for without explaining why in the commit message.
- Leave unrelated code alone.

## Finish

Confirm `git status` is clean and every task is checked, then write the result file the contract describes.
Summarize what changed and list anything a reviewer should look at closely.
