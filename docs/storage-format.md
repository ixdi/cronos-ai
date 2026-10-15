# Local storage format

Cronos-AI stores each project under the storage root supplied to `ProjectStore`:

```text
<root>/projects/<project-uuid>/project.json
<root>/projects/<project-uuid>/tasks/<task-uuid>.json
<root>/locks/store.lock
<root>/locks/<task-uuid>.lock
<root>/extensions/cronos-workflow.ts
<root>/skills/cronos-<step>/SKILL.md
<root>/agent-profiles.json            (optional)
<root>/worktrees/<project-uuid>/<task-uuid>/attempt-<n>/
```

`project.json` contains the project ID, name, optional description, optional canonical local Git repository root, optional check command (program and arguments), optional check time limit in minutes (unset means 60), publish mode (`local`, `push`, or `pull_request`) and optional remote name, active/archived status, task IDs, and creation/update timestamps. Each task file contains its task and project IDs, title, optional description, priority, dependency IDs, workflow status, current step, step timestamps, attempt number, automatic-revision count for the attempt, recent agent activity (bounded to the latest 100 events and tagged by attempt), human approval/revision history (bounded to the latest 100 decisions and tagged by attempt), recovery history (bounded to 50 decisions), an optional abandonment reason, where the work was published (a pull request URL or the pushed branch and remote), optional active worktree reference, and creation/update timestamps. Worktrees are assigned per attempt under `worktrees/<project-id>/<task-id>/attempt-<n>` with a corresponding `cronos-ai/<task-uuid>/attempt-<n>` local branch. Retry/clarification removes the failed worktree and branch; the removed path is retained in recovery history for audit. Older task files without activity, approval, recovery, abandonment, or attempt fields load with empty/default values. IDs are UUIDs; timestamps are seconds since the Unix epoch. Enum values are serialized in `snake_case`. Older project records without a repository field load as unconfigured and must be assigned a Git repository before task execution.

Project and task text is validated before saving and after loading. Individual JSON files are limited to 1 MiB. Writes use a temporary file in the destination directory and atomically replace the destination. Invalid, oversized, symlinked, or identity-mismatched records are rejected; a failed update does not overwrite the existing record.

Every change to projects and tasks holds the store lock (`locks/store.lock`) while it reads, checks, and saves its records, so the TUI, the CLI, and a running workflow step never overwrite each other's changes. Reads do not take it. The operating system releases it when its holder exits, so a crash never leaves the store locked.

A task's lock file is held by the process running one of its workflow steps and is released by the operating system when that process exits; its content is unused. The workflow extension and the built-in step skills are rewritten from the binary when they differ, never through a symlink. Inside each worktree, `.cronos/results/<step>/<agent>.json` holds agent results and `.cronos/check-output.log` the latest check output; `.cronos/` is excluded through the repository's local `info/exclude` and never committed.

Task source code is kept in its Git worktree, not in JSON. The stored worktree reference is metadata and must be validated against the task's allowed worktree root before any later destructive filesystem operation.
