# Workflow state and execution

## Task queue

Cronos starts at most one task at a time. `start_next_task` selects the highest-priority ready Pending task after checking its dependencies. While a task is In Progress or Error, the queue does not start another task. An Error task must be recovered or explicitly abandoned first.

## Ordered steps

1. `specification-generation` — Pi submits independent agent plans to `cronos_run_step`; the Rust workflow engine waits for every agent.
2. `specification-approval` — the human approves or requests revisions in the TUI. Approval advances to code generation; rejection records a note and returns to specification generation while keeping the task In Progress.
3. `code-generation`
4. `code-review`
5. `verification`
6. `final-approval` — the human approves or requests revisions. Approval advances to documentation; rejection returns to code generation and keeps the task In Progress.
7. `documentation`
8. `local-merge` — completion is permitted only after final-code approval and local merge.

The Rust host accepts agent work only for the current agent step. It records Starting and completion activity and advances only after the step barrier succeeds. If an agent fails, siblings are canceled, late results are ignored, and the task becomes Error without advancing. Steps that require human approval cannot be advanced by an agent tool.

## Error recovery

For an Error task, open task details and press **r** to retry from specification generation or **c** to supply clarification for replacement specifications. Clarification is required to be nonblank. Both actions create a new attempt, set status back to In Progress, reset the active step to specification generation, invalidate prior-attempt approvals, and clear the active worktree reference. Agent/approval/recovery history remains tagged and auditable; each attempt is assigned a distinct worktree path, so code from the failed attempt is not the next attempt's workspace. The previous worktree path is retained in recovery metadata for safe Git cleanup.

Each task may keep a validated worktree only at its assigned directory under the Cronos data directory. Pi's project-local extension submits step plans through a narrow internal Rust command; the host validates the task status, current step, approvals, agent names, and worktree before starting Herdr agents. The extension does not expose arbitrary status, approval, or path writes to the model.
