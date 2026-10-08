# Proposal

## Why

Cronos-AI has an architecture and workflow design but no implementation, so users cannot yet create projects and tasks or supervise agent work. A tested, single-task-at-a-time MVP will make the core software-factory loop usable while keeping task-level concurrency out of the first release.

## What Changes

- Add CLI/TUI workflows to create and manage projects and tasks, persist their state, and monitor task progress.
- Execute one task at a time through sequential workflow steps, running independent sub-agents within a step in parallel. Use built-in priority/dependency ordering for the first release.
- Support human approval, step-rejection revision loops, and the agreed error recovery behavior: cancel sibling agents on failure; allow a human to retry from spec generation or clarify context and approve replacement specs.
- Provide basic activity and status visibility, plus the project's required unit, end-to-end, security, and quality checks.
- Keep local worktree and merge operations in scope. Defer automated GitHub pushes; publishing changes to a remote remains a human action.
- Defer parallel execution of multiple tasks, advanced dashboard analytics/polish, and Jev integration. Jev support will be added later through a Pi extension.

## Capabilities

### New Capabilities
- `project-management`: Create and manage projects and their tasks, persist task data and statuses, and provide basic TUI visibility.
- `task-workflow`: Execute a task through ordered steps with parallel independent sub-agents, human approvals, failure handling, retry, and clarification.

### Modified Capabilities

None.

## Impact

This is a greenfield implementation: the repository currently contains only project guidance, design documentation, and OpenSpec scaffolding. Expected areas include the CLI/TUI, project and task JSON storage, workflow orchestration, agent/tool integrations (Pi, Herdr, OpenSpec, and the sub-agent extension), Git worktrees/local merge, and automated tests and quality/security checks. Remote GitHub push automation and Jev integration are out of scope for this release; Jev will be integrated later through a Pi extension.
