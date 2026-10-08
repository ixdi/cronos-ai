# Project and task commands

Cronos-AI starts the TUI when invoked without a subcommand. Project and task management is also available from the CLI:

```text
cronos-ai [--data-dir DIR] project create <name> [--description <text>]
cronos-ai [--data-dir DIR] project list
cronos-ai [--data-dir DIR] project select <project-id>
cronos-ai [--data-dir DIR] project update <project-id> [--name <name>] [--description <text>]
cronos-ai [--data-dir DIR] project archive <project-id>
cronos-ai [--data-dir DIR] task create <project-id> <title> [--description <text>] [--priority low|medium|high] [--depends-on <task-id[,task-id...]>]
cronos-ai [--data-dir DIR] task list <project-id>
cronos-ai [--data-dir DIR] task ready <project-id>
```

Commands print the affected record or result as JSON. `project select` prints an active project for the current command; it does not persist a global selection. Archived projects remain visible in `project list` but cannot be selected for active work or receive new tasks. A task dependency must refer to another task in the same project. `task ready` returns pending tasks whose dependencies are all completed, ordered by priority (high first) and then creation time.

If `--data-dir` is omitted, Cronos-AI uses `CRONOS_AI_DATA_DIR`, then the platform's user data directory (`XDG_DATA_HOME` or `~/.local/share` on Unix-like systems, `APPDATA` on Windows), under `cronos-ai`.

The initial TUI scaffold currently displays the application shell and exits with `q` or `Esc`; project/task navigation and activity views are tracked separately in the OpenSpec change.
