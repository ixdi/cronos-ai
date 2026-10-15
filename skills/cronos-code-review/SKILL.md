---
name: cronos-code-review
description: Review a Cronos-AI task branch against its OpenSpec change and report concrete, verified findings without modifying anything. Use in the code-review step.
---

# Code review

You review the task branch read-only.
Your findings send the code back to the code-generation agent, so each one must be concrete enough to act on without asking you.
The Cronos-AI contract at the end of your prompt names the change, the result file, and the rules you must follow; it always wins over this skill.

## Gather context

1. Read the OpenSpec change (or its archived copy) to know what the branch must do.
2. Read the branch's changes against its merge base, for example `git log --oneline` and `git diff <base>...HEAD`.
3. Read enough of the surrounding code to judge each change in context.

Never modify, stage, commit, or stash files, and never run commands that write to the worktree.

## What to check

- **Spec conformance:** every requirement and scenario is implemented, and nothing outside the change was added.
- **Correctness:** logic errors, off-by-one errors, unhandled errors, race conditions, resource leaks, and broken edge cases such as empty, very large, or invalid input.
- **Tests:** each scenario has a test that would fail without the change; tests check behavior rather than implementation details; no test was weakened or skipped.
- **Security:** untrusted input reaching shells, file paths, queries, or templates; secrets in code or logs; unsafe permissions.
- **Maintainability:** code that duplicates existing helpers, misleading names, dead code, or comments that contradict the code.

## Reporting

- Verify each finding before reporting it: point to the file and line, describe the failing input or scenario, and say what should change.
- Report `changes_requested` only for problems that must be fixed before merging.
  Mention minor style preferences in the summary, not as findings.
- Report `success` when the branch is ready, even if you have optional suggestions.
- Use `failed` only when you cannot review at all, for example because the branch or change is missing.
