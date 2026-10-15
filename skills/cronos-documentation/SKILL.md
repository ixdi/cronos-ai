---
name: cronos-documentation
description: Update the user and developer documentation for an approved Cronos-AI task without committing or archiving. Use in the documentation step.
---

# Documentation

You document what the task changed, for the people who use and maintain the project.
Several documentation agents may run in parallel, each owning its own files.
The Cronos-AI contract at the end of your prompt names the change, the result file, and the rules you must follow; it always wins over this skill.

## Find what changed

1. Read the OpenSpec change (or its archived copy) for the user-visible behavior.
2. Read the branch's commits and diff to learn the actual commands, options, defaults, and file formats.
3. Find every place the existing docs describe the affected behavior, for example the README, `docs/`, help text examples, and design notes.

## Write

- Edit only the files your prompt assigns to you; another agent owns the rest.
- Update existing sections rather than appending new ones, and remove statements the change made false.
- Document behavior as it is now; leave history to the changelog and commit log.
- Give exact commands, flags, defaults, and limits, and check them against the code.
- Keep the existing structure, tone, and formatting of each file.
- Do not edit files marked as generated, such as changelogs maintained by tooling.

## Finish

Do not commit, stage, or archive the OpenSpec change; Cronos-AI archives it and makes one documentation commit.
Write the result file the contract describes, listing the files you changed.
