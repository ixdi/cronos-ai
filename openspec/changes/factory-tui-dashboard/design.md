# Design

## Context

See proposal.md for motivation and factory-monitoring-tui/spec.md for the user-visible contract. The CLI currently uses `argparse` and exposes aggregate status, while `FactoryStore` already persists runs, plans, task state, worker assignments, attempts, review packets, controller status, and paths in `RunContext`. `HumanAttentionQueue` derives actionable human decisions. Worker progress is currently summarized and sent to Herdr but is not retained as a browsable local event history. The package supports Python 3.12+, and has no existing terminal-UI dependency or commitlint configuration.

## Goals / Non-Goals

**Goals:**
- Add a responsive terminal dashboard on top of existing factory state and domain services, with no duplicate lifecycle state.
- Persist a concise, safe event history that makes worker progress and state transitions inspectable after restarts.
- Make progress and stage calculations deterministic and visible to operators.
- Keep the dashboard non-mutating and keep the implementation isolated in a dedicated Git worktree with Conventional Commit messages.

**Non-Goals:**
- Add a web dashboard, remote multi-user access, or a new monitoring service.
- Trigger approvals, retries, conflict resolution, or any other factory action from the dashboard.
- Estimate remaining wall-clock duration or expose unrestricted worker/tool transcripts.
- Automatically open, edit, or execute generated paths from the dashboard.

## Decisions

### Use Textual for the interactive terminal UI

Add Textual as the TUI framework and place the dashboard screens behind a small application/presentation module, leaving CLI parsing and domain/storage APIs independent of Textual. Textual provides keyboard-driven navigation, reactive refresh, and terminal resizing without requiring a bespoke input/rendering loop. The alternatives are a standard-library `curses` interface, which avoids a runtime dependency but has less consistent terminal/platform behavior and requires more low-level UI work, or a plain `Rich` display, which is less suitable for persistent navigation and detail panels. Pin the compatible range in project metadata and lockfile, and include the new dependency in normal dependency review/audit.

### Compose a read model from existing durable state

Create dashboard snapshots through an explicit query/service boundary that combines run and plan records, task records, attempts, controller status, run context, review packets, and `HumanAttentionQueue` items. Avoid writes to factory lifecycle tables during viewing or refresh. Use run identifiers as stable selection keys and retain the existing `--state-dir` override. Keep formatting and stage/progress derivation out of the CLI argument handler.

Derive a run's completion percentage from `DONE` task records divided by planned tasks, rounded to a stable whole percent, and display the numerator and denominator. Never infer duration. Determine the active stage from the kinds of currently active plan tasks; when none is active, show the kind of the next incomplete planned task. If stage cannot be resolved from persisted plan data, display an explicit unknown value. Display run state separately from stage so blocked or human-waiting work is not presented as active implementation.

### Persist concise activity events in SQLite

Add an additive SQLite migration for an indexed activity-event table containing an event identifier, run and optional task identifiers, UTC timestamp, event category, and a bounded human-readable summary. Query the newest events by run/task with cursor or limit-based pagination so the UI only loads the visible window. Record lifecycle transitions and the existing concise worker progress summaries at their durable event boundaries. Do not persist raw Pi RPC records, MCP/tool arguments or results, environment values, or arbitrary process output. Apply secret-pattern redaction and length limits to summaries before persistence and before rendering; cover known token forms with tests and fail closed by suppressing an event if sanitization fails.

Keep activity history separate from attempts: attempts remain the authoritative structured record of execution outcomes, while events provide navigable progress context. Existing databases are upgraded by the normal versioned `FactoryStore` migration path. Rollback is code rollback only: the additive table can remain unused without destructive downgrade or loss of factory state.

### Refresh without blocking the interface

Run periodic state refresh at a modest configurable/default interval, coalesce redundant refresh requests, and render only changed snapshots. Keep database reads and event-page loads bounded; perform potentially slow reads away from the UI event loop. Handle a stopped controller as a valid observable state, and keep transient read errors visible without discarding the last successfully rendered snapshot.

### Keep dashboard interaction read-only

Dashboard key actions select runs/tasks, change detail tabs, refresh, and exit. The existing CLI action commands remain the only way to enqueue approval, retry, clarification, or conflict-resolution actions. Tests should verify that opening, navigating, and refreshing leave requests, tasks, attempts, approvals, and control actions unchanged.

### Work in an isolated worktree and use compliant commits

For implementation, create a dedicated Git worktree from the intended base before editing code and work only there. Keep the OpenSpec planning artifacts as the source-of-truth plan and ensure they are present in the implementation worktree as appropriate to the branch workflow. Make focused commits with Conventional Commit subjects accepted by commitlint; first inspect the repository/CI for an actual commitlint configuration, and if none exists, use the established `type(scope): description` convention without introducing unrelated tooling configuration. Do not commit local factory databases, generated runtime logs, credentials, or temporary worktree state.

## Risks / Trade-offs

- [Risk] Textual adds a runtime dependency and its supported terminal behavior can vary → Keep UI code isolated, test with supported terminal sizes, and run dependency lock, lint, test, build, and audit checks.
- [Risk] Progress summaries can contain provider or user secrets → Persist only concise event summaries, redact and cap content before storage and rendering, omit raw tool/process payloads, and test representative secret formats.
- [Risk] Frequent refreshes can contend with the controller's SQLite access → Use short, indexed snapshot queries, bounded event pages, a conservative refresh interval, and no long-lived write transaction.
- [Risk] Task-count completion can be mistaken for a time estimate → Always label the percentage as completed tasks and show the completed/total count; show no remaining-time estimate.
- [Risk] Run paths may be removed or inaccessible → Treat paths as references, render availability errors per output, and keep the rest of the dashboard usable.

## Migration Plan

1. Implement and test the additive SQLite event migration and event write/query APIs without changing existing state semantics.
2. Add the read-model calculations and dashboard UI, then expose the interactive command and document usage.
3. Validate existing and newly migrated state directories, including empty databases and concurrent controller/dashboard access; run the project's full test, lint, type-check, build, and dependency-audit checks in the implementation worktree.
4. Roll back by reverting the dashboard code and dependency if necessary. Leave the unused additive event table in place; do not delete factory state or attempt a destructive downgrade.
