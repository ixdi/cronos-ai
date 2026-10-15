---
name: cronos-verification
description: Verify a Cronos-AI task branch by running its tests and checking security and quality, without modifying anything. Use in the verification step, before the project check command and final human approval.
---

# Verification

You confirm the task branch works, read-only.
After you, Cronos-AI runs the project's check command, and then a human gives final approval.
The Cronos-AI contract at the end of your prompt names the change, the result file, and the rules you must follow; it always wins over this skill.

## Run what proves the change works

1. Read the OpenSpec change (or its archived copy) and list the scenarios it promises.
2. Find how the project builds and tests itself, for example its README, `package.json`, `Cargo.toml`, `Makefile`, or `scripts/`.
3. Run the test suites, including end-to-end tests, and confirm each scenario is exercised by a test that passes.
4. Run the project's linters and type checkers if it has them.
5. Run the project's dependency audit if it has one.

Never modify, stage, commit, or stash files.
If a command would write build output into the worktree, run it with an output directory outside the worktree or skip it and say so.

## What to look for

- Failing, flaky, or skipped tests, and scenarios without a test.
- Security problems: untrusted input reaching shells, file paths, queries, or templates; secrets in code or logs; vulnerable dependencies.
- Behavior that differs from the spec when the program is used as a user would use it.

## Reporting

- For each finding, give the command you ran, the relevant output, and what must change.
- Report `changes_requested` when anything must be fixed; report `success` when everything you ran passed.
- Use `failed` only when you cannot verify at all, for example because the project cannot be built for reasons outside the branch.
