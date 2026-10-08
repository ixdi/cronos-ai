# Design

## Context

See `proposal.md` for motivation and `specs/` for externally visible behavior. The repository has no application source or build configuration yet. The project design calls for a Rust `ratatui` CLI/TUI, JSON project/task storage, Pi-based agents, Herdr, OpenSpec, and Git worktrees. Local inspection found Rust, Node, Pi, and Herdr installed; Jev is absent and deferred to a future Pi extension. Pi exposes JSON/RPC output modes, and Herdr exposes a socket API, but no project adapter exists to validate their exact contracts.

## Goals / Non-Goals

**Goals:**
- Establish a small Rust application with a testable core, basic ratatui project/task screens, and persistent workflow state.
- Run one task at a time through ordered steps, with independent agents concurrent within a step.
- Make approvals, failure cancellation, recovery, and local-only completion explicit in workflow state.
- Keep external agent/tool integrations replaceable and testable without live services in unit tests.

**Non-Goals:**
- Concurrent execution of multiple tasks.
- Automated remote GitHub pushes, advanced dashboard analytics, or polished visual customization.
- Silent installation of missing tools or automatic retries after failure.
- Jev integration; a later change will provide a Pi extension for it.

## Decisions

### Rust CLI/TUI owns durable state and workflow control

Use a Rust binary with `ratatui` for the interface. Keep project/task models, status transitions, the task queue, and workflow-step barriers in the application core; the TUI renders that state and sends user actions to it. This keeps state transitions testable without a terminal. A Pi-only workflow was considered, but would make durable project state and the Rust TUI depend on an agent session.

### JSON is the local source of truth

Use one project JSON file plus per-task JSON files, as described in the project design. Persist status, priorities, dependencies, current step, worktree reference, and attempt metadata. Write updates through a temporary file followed by atomic rename so interruption cannot leave a partially written JSON document. SQLite was considered but is unnecessary for this single-user, local MVP and would depart from the existing design.

### Integrate tools behind narrow process/API adapters

Keep Pi, Herdr, OpenSpec, and Git behind small adapters owned by the workflow core. Use Pi's machine-readable JSON/RPC interface and Herdr's local API where their installed versions support the required operations; invoke OpenSpec and Git through their documented CLI interfaces. Keep domain state in Cronos-AI rather than treating tool sessions as the source of truth. Validate agent launch, result collection, and cancellation in an early compatibility task; provide fake adapters for automated tests. A missing required tool must be reported and the user prompted before the affected workflow proceeds; never install it silently. Jev is not required by this release; a separate Pi extension will integrate it later.

### Execute steps serially and agents within a step concurrently

The workflow runner processes one task at a time, selecting ready tasks by dependency readiness and priority using built-in deterministic ordering. It completes each task step before starting the next, while scheduling independent agents for the active step concurrently and joining their results at a barrier. On the first agent failure, request cancellation for the other running agents, stop advancing, persist Error, and wait for a human decision. Cancellation is best-effort at external process/API boundaries; late results are ignored and cannot advance the task. Jev-based prioritization is deferred to the future Pi extension.

### Human recovery starts a fresh specification-based attempt

On retry, restart at specification generation and discard outputs downstream of the specifications from the previous attempt rather than reusing them. Keep attempt/status metadata for task history, but do not retain prior downstream outputs as active task artifacts. For clarification, create or replace the specifications from the updated context and require approval before continuing to code generation. Once the human's choice resumes processing, transition Error to In Progress. Approval-step rejection follows its revision loop and does not set the terminal task status Rejected.

### Keep work local after approval

Create a Git worktree per task. After final approval, documentation, and successful local merge, mark the task Completed. Do not push to a remote in this release; remote publishing remains a human action. The automated push step in the project design is deferred.

### Test through core and adapter boundaries

Use unit tests for state transitions, dependency/priority selection, serialization, and workflow barriers. Use temporary project directories and fake tool adapters for integration and end-to-end tests, including failure cancellation, retry/clarification, approval loops, and no-push completion. Keep the required security and code-quality checks in the build/test gate.

## Risks / Trade-offs

- [Pi/Herdr CLI or API contracts differ across installed versions] -> Isolate them behind adapters and make compatibility verification the first integration task; prompt for missing tools rather than assuming availability.
- [External agents may not stop immediately after cancellation] -> Treat cancellation as best-effort, keep the task in Error, ignore late outputs, and require a human recovery decision.
- [JSON writes can be interrupted or user-edited] -> Use atomic replacement, validate data when loading, and report invalid project/task files without overwriting them.
- [Local merge can fail due to conflicts] -> Leave the task uncompleted and surface the conflict for human resolution; never push automatically.

## Migration Plan

This is a greenfield application with no existing project data to migrate. Create the initial project/task JSON format with the application and keep its first-release format local to this change. No remote repository migration or push is performed.
