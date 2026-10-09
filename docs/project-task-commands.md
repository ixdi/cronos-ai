# Project and task commands

Cronos-AI starts the TUI when invoked without a subcommand. Project and task management is also available from the CLI:

```text
cronos-ai [--data-dir DIR] project create <name> [--description <text>] [--repository <PATH>]
cronos-ai [--data-dir DIR] project set-repository <project-id> --repository <PATH>
cronos-ai [--data-dir DIR] project list
cronos-ai [--data-dir DIR] project select <project-id>
cronos-ai [--data-dir DIR] project update <project-id> [--name <name>] [--description <text>]
cronos-ai [--data-dir DIR] project archive <project-id>
cronos-ai [--data-dir DIR] task create <project-id> <title> [--description <text>] [--priority low|medium|high] [--depends-on <task-id[,task-id...]>]
cronos-ai [--data-dir DIR] task list <project-id>
cronos-ai [--data-dir DIR] task ready <project-id>
```

Commands print the affected record or result as JSON. `project select` prints an active project for the current command; it does not persist a global selection. Archived projects remain visible in `project list` but cannot be selected for active work or receive new tasks. A task dependency must refer to another task in the same project. `task ready` returns pending tasks whose dependencies are all completed, ordered by priority (high first) and then creation time.

## Create a project from the TUI

On the project list, press **n** to open the project form. Enter a name; description and a local Git repository path are optional. Use **Tab** or **Shift+Tab** to move between fields, **Enter** to create the project, or **Esc** to cancel. A successful project is saved as active and selected. Validation errors leave the form open with its values so they can be corrected; cancelling creates nothing.

## Create a task from the TUI

Open an active project's task list and press **n**. Enter a title and optional description. Priority defaults to **Medium**; use **Tab** to focus it and **←/→** to change it. In the dependencies field, use **↑/↓** to choose same-project tasks and **Space** to toggle them. Press **Enter** to create the task or **Esc** to cancel. The new task is **Pending** and is not started automatically; press **s** separately to start the next ready task. Archived projects cannot receive tasks.

Configure a local Git repository when creating a project, or use `project set-repository` before starting work. Cronos-AI resolves and stores the repository root. Existing projects with only pending/error tasks and no active worktrees may be configured later.

Starting a task creates a Git worktree at `worktrees/<project-id>/<task-id>/attempt-<n>` under the Cronos data directory and uses a task/attempt-specific local branch. A retry or clarification removes the failed attempt's worktree and branch before creating a fresh attempt. After final-code approval, documentation runs in that worktree; Rust performs the local merge only after Pi settles. Cronos-AI never pushes. Publishing remains a manual Git operation for the user. Merge conflicts are aborted so the base worktree is left unchanged and the task is marked Error.

## End-to-end workflow

1. Start with a clean local Git repository and configure `user.name`/`user.email` so the documentation agent can commit the task output.
2. Run `cronos-ai project create demo --repository /path/to/repo`, then create a task with `cronos-ai task create <project-id> "Build the feature"`. The JSON results contain the IDs.
3. Start `cronos-ai` inside a Herdr-managed pane. Select the project with **Enter**; press **n** on its task list to create a task, or **s** to start the highest-priority ready task. Open a task with **Enter** to follow its activity.
4. At specification and final-code approval gates, review the output and press **y** to approve. Press **n** to request a revision and supply a nonblank note. If an attempt fails, **r** discards and retries from specifications; **c** supplies clarification and regenerates them.
5. After final-code approval, agents generate and commit documentation. Cronos-AI verifies that the base repository and task worktree are clean, merges the task branch locally, and marks the task Completed. It does not push; publish manually if desired.

The full temporary-repository path (including a failed parallel-agent attempt, recovery, approvals, documentation commit, and local merge) is exercised by `cargo test --test workflow_e2e`. If `--data-dir` is omitted, Cronos-AI uses `CRONOS_AI_DATA_DIR`, then the platform's user data directory (`XDG_DATA_HOME` or `~/.local/share` on Unix-like systems, `APPDATA` on Windows), under `cronos-ai`.
