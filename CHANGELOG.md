# Changelog

Notable changes to Cronos-AI are recorded here. The package version is `0.1.0`.

## Unreleased

### Added

- Rust CLI and Ratatui dashboard for project/task management and workflow monitoring.
- Persisted project and task state, priority/dependency ordering, bounded agent activity, and human approval/revision history.
- Pi RPC workflow extension with read-only task-state queries and Rust-host-validated state transitions.
- Sequential task workflow with parallel agents per step, sibling cancellation, error recovery, retries, and clarification-driven specification regeneration.
- Per-attempt Git worktrees and branches, approval-gated documentation, and local merge with conflict abort. Remote pushes are not performed.
- Tool preflight, security/quality checks, workflow documentation, and temporary-repository end-to-end tests.
