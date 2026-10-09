# Monitoring Cronos-AI work

Start `cronos-ai` without a subcommand to open the dashboard. The dashboard checks for `pi`, `herdr`, `openspec`, and `git` first; if any are missing, it asks you to install them and press Enter to retry. It never installs tools automatically.

## Navigation

- **Projects:** use ↑/↓ to select a project and Enter to open its task list. Active and archived projects are shown so archived work remains inspectable.
- **Tasks:** use ↑/↓ to select a task. Each row shows status, priority, and current workflow step. Press Enter for details. Press **s** to start the next priority/dependency-ready task; task execution requires a validated Git worktree and a Herdr-managed session.
- **Task details:** the summary shows status, priority, attempt, description, and active step. Human decisions, recovery context, and revision notes appear above the agent activity panel; use ↑/↓ to select an activity update.
- For an **Error** task, press **r** to discard the failed attempt and retry specification generation or **c** to enter clarification context and regenerate specifications. Recovery removes that attempt's worktree and local branch, resumes In Progress on a fresh worktree, and does not carry approvals forward. Press **a** to abandon instead.
- At the specification or final-code approval step, press **y** to approve or **n** to request revisions and enter a note. A revision returns the workflow to specification generation or code generation without changing the task from In Progress. Final-code approval unlocks documentation; Rust merges the completed task branch locally after Pi settles. No remote push is performed.
- Press **a** in task details to abandon the task; enter a reason to mark it Rejected. Backspace/Esc cancels an open dialog or returns one level; **r** refreshes immediately and **q** exits.

The dashboard refreshes persisted project/task state once per second, so status, step, and activity updates written by the workflow host appear without restarting. Activity is kept per task with a 100-event limit; older task files without activity data remain readable and show an empty activity panel.
