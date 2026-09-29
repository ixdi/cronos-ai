# Proposal

## Why

Cronos AI can persist multiple concurrent factory runs, task states, worker assignments, attempts, and review evidence, but its current `status` command reports only aggregate controller and queue counts. Operators need a live, navigable view to understand what each factory is doing, identify stalls or human decisions, and inspect useful output without querying SQLite or switching among commands.

## What Changes

- Add an interactive terminal dashboard command to inspect all factory runs and the controller's health, with refreshable status and clear empty, loading, and error states.
- Show each run's current workflow stage, task states, assigned workers, elapsed activity, and a meaningful completion estimate/percentage derived from persisted task progress, with the calculation and unknown states made explicit.
- Provide run and task detail views for persisted activity logs/progress events, errors and attempt history, human-attention items, review summaries, and links/paths to generated plans and run worktrees.
- Preserve the existing non-interactive CLI commands and support an explicit state-directory override for the dashboard.
- Keep monitoring read-only: approvals, retries, conflict resolution, and other mutations continue through the existing action commands.
- Implement the feature in a dedicated Git worktree and keep implementation commits aligned with the repository's Conventional Commits/commitlint policy; do not commit secrets or runtime logs.

## Capabilities

### New Capabilities
- `factory-monitoring-tui`: Interactive, read-only monitoring of concurrent factory runs, workflow progress, task and worker state, activity/error history, human attention, and generated outputs.

### Modified Capabilities

None.

## Impact

The `cronos-ai` CLI and Python package; read access to `FactoryStore`, run/task/attempt records, controller status, human-attention items, and review packets; new durable progress/activity data to support log inspection; TUI dependency and terminal compatibility; documentation and tests. Existing capabilities for application identity and local-state migration were reviewed and do not need requirement changes. The repository currently has no configured TUI framework or commitlint configuration; use the project's Conventional Commits guidance and confirm the applicable commitlint setup during implementation.
