# Cronos-AI

![Cronos-AI — supervised software factory](assets/cronos-ai-banner.svg)

Cronos-AI is a supervised software factory for taking a project task through a structured, agent-assisted workflow. It is a Rust CLI/TUI: Rust owns persisted task state and human approval gates, Pi orchestrates workflow steps, and Herdr manages the agents that work in parallel within a step.

## What it does

- Manage projects and prioritized, dependency-aware tasks from the CLI.
- Monitor task status and agent activity in a terminal dashboard.
- Run one task at a time through specification, implementation, review, verification, approval, documentation, and local merge steps.
- Require human approval of specifications and final code; request revisions or recover failed attempts with additional context.
- Give each attempt an isolated local Git worktree. After final approval, changes are merged locally; Cronos-AI never pushes to a remote.

The MVP does not run multiple tasks concurrently. Jev-based prioritization and automated remote publishing are deferred.

## Requirements

- Rust stable (the toolchain is specified in `rust-toolchain.toml`).
- Git, Pi, Herdr, and OpenSpec for the supervised workflow. On startup, Cronos-AI checks for required tools and prompts if any are missing; it does not install them automatically.
- A local Git repository for each project that will run tasks. Keep its main working tree clean and configure `user.name` and `user.email` so workflow agents can commit their output.

## Build and run

```sh
cargo build --release
```

Create a project linked to a repository and add a task:

```sh
./target/release/cronos-ai project create demo --repository /path/to/repository
./target/release/cronos-ai task create <PROJECT_ID> "Build the feature" --priority high
```

The commands print JSON containing the project and task IDs. Start the dashboard from a Herdr-managed pane:

```sh
./target/release/cronos-ai
```

Select the project with **Enter**, press **s** on its task list to start the next ready task, then open the task to monitor progress. Review and approve the specification and final-code gates in the TUI. See [the workflow guide](docs/project-task-commands.md) for recovery, navigation, and CLI details.

For existing projects, set the repository before starting work:

```sh
./target/release/cronos-ai project set-repository <PROJECT_ID> --repository /path/to/repository
```

The data directory can be selected with `--data-dir DIR` or `CRONOS_AI_DATA_DIR`. Otherwise, Cronos-AI uses the platform's user data directory. See [storage format](docs/storage-format.md).

## Development

```sh
cargo test
./scripts/check.sh
```

The check script runs formatting, Clippy, all targets' tests, and `cargo audit`. The end-to-end workflow test uses a temporary local Git repository and fake agent adapter:

```sh
cargo test --test workflow_e2e
```

## Documentation

- [Project/task commands and end-to-end use](docs/project-task-commands.md)
- [TUI monitoring and controls](docs/monitoring.md)
- [Workflow states and recovery](docs/workflow-states.md)
- [Storage format](docs/storage-format.md)
- [Pi, Herdr, and Git integration contracts](docs/integration-contracts.md)
- [System design](design/design.md)
- [Changelog](CHANGELOG.md)
