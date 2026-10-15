# Project and task commands

Cronos-AI starts the TUI when invoked without a subcommand. Project and task management is also available from the CLI:

```text
cronos-ai [--data-dir DIR] project create <name> [--description <text>] [--repository <PATH>]
cronos-ai [--data-dir DIR] project set-repository <project-id> --repository <PATH>
cronos-ai [--data-dir DIR] project set-check-command <project-id> -- <program> [args...]
cronos-ai [--data-dir DIR] project set-check-timeout <project-id> [minutes]
cronos-ai [--data-dir DIR] project set-publish <project-id> <local|push|pull-request> [--remote <name>]
cronos-ai [--data-dir DIR] project set-releases <project-id> <on|off>
cronos-ai [--data-dir DIR] project set-parallelism <project-id> <1-8>
cronos-ai [--data-dir DIR] project set-sandbox <project-id> <on|off>
cronos-ai [--data-dir DIR] project set-prioritizer <project-id> <built-in|agent> [--model <model>]
cronos-ai [--data-dir DIR] project set-planner-model <project-id> [model]
cronos-ai [--data-dir DIR] project list
cronos-ai [--data-dir DIR] project select <project-id>
cronos-ai [--data-dir DIR] project update <project-id> [--name <name>] [--description <text>]
cronos-ai [--data-dir DIR] project archive <project-id>
cronos-ai [--data-dir DIR] task create <project-id> <title> [--description <text>] [--priority low|medium|high] [--depends-on <task-id[,task-id...]>]
cronos-ai [--data-dir DIR] task list <project-id>
cronos-ai [--data-dir DIR] task ready <project-id>
cronos-ai [--data-dir DIR] task rank <project-id> [--timeout-seconds <1-1800>]
cronos-ai [--data-dir DIR] requirement add <project-id> <text> [--kind functional|non-functional]
cronos-ai [--data-dir DIR] requirement list <project-id>
cronos-ai [--data-dir DIR] requirement remove <project-id> <requirement-id>
cronos-ai [--data-dir DIR] project plan <project-id> [--timeout-minutes <1-480>]
cronos-ai [--data-dir DIR] project plan-show <project-id>
cronos-ai [--data-dir DIR] project plan-apply <project-id>
cronos-ai [--data-dir DIR] project plan-discard <project-id>
```

## Plan tasks from requirements

Requirements describe what the product must do (`functional`, the default) or how well (`non-functional`, such as performance or security budgets). Their text is trimmed and whitespace-collapsed, and a requirement that matches an existing one, ignoring case and spacing, is rejected.

`project plan` runs Pi once in the project repository: read-only (`--tools read,grep,find,ls`), with Pi's own discovery off, project-local Pi files ignored, and only the built-in `cronos-project-planning` skill. The planner reads the README, the code, and `openspec/specs/`, then answers with a JSON plan. Cronos rejects a plan that leaves a requirement uncovered, refers to unknown requirements or tasks, has a dependency cycle, repeats a title, or proposes more than 50 tasks, and keeps a valid one pending; it stops the planner after `--timeout-minutes` (30 by default). Nothing is created until `project plan-apply`, which creates the tasks dependencies first, links each to the requirements it covers, and reuses an open task with the same title. `project plan-discard` throws the plan away. Planning needs the project's repository and at least one requirement.

Commands print the affected record or result as JSON. `project set-check-command` stores the program and its arguments separately; the check runs without a shell in the task worktree after the verification agents, and a relative program such as `./scripts/check.sh` resolves inside the worktree. `project set-check-timeout` sets how many minutes (1 to 480) the check may run before it is stopped; omit the minutes to restore the 60-minute default. `project set-publish` chooses how completed tasks are published: `local` (default) merges only on this machine, `push` merges and then pushes the base branch, and `pull-request` pushes the task branch and opens a pull request with the GitHub CLI (`gh`, authenticated with `gh auth login`) instead of merging. `--remote` names the Git remote (default `origin`). `project set-parallelism` sets how many tasks may run at the same time; tasks sharing a requirement never run together. `project set-prioritizer agent` lets a tool-less Pi agent (on `--model`, for example a fast selection model) decide which startable task goes first; it only reorders what the scheduling rules allow and falls back to the built-in order when it fails. `project set-planner-model` chooses the model of the step planner, which proposes each step's agents, and of `project plan`; omit the model to use Pi's default. `task rank` shows the start order, and the agent's reason, without starting anything. `project set-sandbox on` runs every worker in a write sandbox that protects Cronos data, the base checkout, and Git hooks and configuration; it needs `/usr/bin/sandbox-exec` on macOS or `bwrap` on Linux, and steps refuse to start without it (see the [trust boundary](integration-contracts.html#trust-boundary)). `project set-releases on` makes every local merge a release: Cronos bumps the version from the Conventional Commits since the last `vX.Y.Z` tag (breaking change: major, `feat`: minor, anything else: patch), prepends the changes to `CHANGELOG.md`, commits `chore(release): vX.Y.Z`, and tags it; with `push` publishing the tag is pushed too. Pull requests are not released, because they merge elsewhere. `project select` prints an active project for the current command; it does not persist a global selection. Archived projects remain visible in `project list` but cannot be selected for active work or receive new tasks. Task titles are trimmed and whitespace-collapsed, and a task whose title matches an open task (one that is not Completed or Rejected), ignoring case and spacing, is rejected. A task dependency must refer to another task in the same project. `task ready` returns pending tasks whose dependencies are all completed, ordered by priority (high first) and then creation time.

## Create a project from the TUI

On the project list, press **n** to open the project form. Enter a name; description, a local Git repository path, and a check command are optional when saving, but tasks only start once the repository and check command are set. The check command field is split on whitespace into a program and its arguments. Use **Tab** or **Shift+Tab** to move between fields, **Enter** to create the project, or **Esc** to cancel. A successful project is saved as active and selected. Validation errors leave the form open with its values so they can be corrected; cancelling creates nothing.

## Create a task from the TUI

Open an active project's task list and press **n**. Enter a title and optional description. Priority defaults to **Medium**; use **Tab** to focus it and **←/→** to change it. In the dependencies field, use **↑/↓** to choose same-project tasks and **Space** to toggle them. Press **Enter** to create the task or **Esc** to cancel. The new task is **Pending** and is not started automatically; press **s** separately to start the next ready task. Archived projects cannot receive tasks.

Configure a local Git repository when creating a project, or use `project set-repository` before starting work. Cronos-AI resolves and stores the repository root. Existing projects with only pending/error tasks and no active worktrees may be configured later. The repository must contain a committed `openspec/` directory (run `openspec init` and commit it); otherwise starting a task discards the new worktree and reports the problem.

Starting a task creates a Git worktree at `worktrees/<project-id>/<task-id>/attempt-<n>` under the Cronos data directory and uses a task/attempt-specific local branch. A step retry keeps it; a retry or clarification removes the failed attempt's worktree and branch before creating a fresh attempt. After final-code approval, documentation runs in that worktree; Rust archives the OpenSpec change, makes the documentation commit, and performs the local merge after Pi settles. Cronos-AI never pushes. Publishing remains a manual Git operation for the user. A merge conflict is aborted, leaving the base unchanged, and the task returns to code generation to resolve it; any other merge failure is aborted and marks the task Error.

## End-to-end workflow

1. Start with a clean local Git repository with a committed `openspec/` directory, and configure `user.name`/`user.email` so agents and Cronos can commit.
2. Run `cronos-ai project create demo --repository /path/to/repo`, then `cronos-ai project set-check-command <project-id> -- ./scripts/check.sh`, then create a task with `cronos-ai task create <project-id> "Build the feature"`. The JSON results contain the IDs.
3. Start `cronos-ai` inside a Herdr-managed pane. Select the project with **Enter**; press **n** on its task list to create a task, or **s** to start the highest-priority ready task. Open a task with **Enter** to follow its activity.
4. At specification and final-code approval gates, press **v** to review the work, then **y** to approve or **n** to request a revision with a nonblank note. If an attempt fails, **t** retries the failed step, **r** discards and retries from specifications, and **c** supplies clarification and regenerates them.
5. After final-code approval, agents write documentation and Cronos makes the documentation commit. Cronos-AI verifies that the base repository and task worktree are clean, merges the task branch locally, and marks the task Completed. With `project set-publish`, it can instead push the merge or open a pull request.

The full temporary-repository path (including a failed parallel-agent attempt, step retry, automatic revisions, approvals, the documentation commit, and local merge) is exercised by `cargo test --test workflow_e2e`; `cargo test --test project_management_e2e` drives these CLI commands through the binary. If `--data-dir` is omitted, Cronos-AI uses `CRONOS_AI_DATA_DIR`, then the platform's user data directory (`XDG_DATA_HOME` or `~/.local/share` on Unix-like systems, `APPDATA` on Windows), under `cronos-ai`.
