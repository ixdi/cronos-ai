<p align="center">
<img src="assets/cronos-ai-banner.svg" alt="Cronos-AI" width="80%"/>
</p>

# Cronos-AI

Cronos-AI is a supervised software factory for taking a project task through a structured, agent-assisted workflow.

It is an opinionated stack with a Rust TUI. Rust owns persisted task state and human approval gates, Pi orchestrates workflow steps, and Herdr manages the agents that work in parallel within a step.

> [!WARNING]
> The MVP is not yet capable of running multiple tasks concurrently. Jev-based prioritisation has been deferred.

## What it does

- Manage projects and prioritized, dependency-aware tasks from the CLI and TUI.
- Monitor task status and agent activity in a terminal dashboard.
- Manage named Pi skill, extension, MCP, model, and time-limit profiles for workflow agents from the TUI.
- Run one task at a time through specification, implementation, review, verification, approval, documentation, and local merge steps. Agents report structured results; Rust decides each step from them and runs the project's check command before final approval.
- Send code back for automatic revision (up to 3 per attempt) when reviewers, verifiers, or the check command find problems.
- Require human approval of specifications and final code, with a review view of the spec, commits, diff, and check output; request revisions, retry a failed step, or recover failed attempts with additional context.
- Give each attempt an isolated local Git worktree. After final approval, changes are merged locally; Cronos-AI never pushes to a remote.

![Workflow diagram](design/workflow.svg)

The diagram shows the target model, where several feature tracks run in parallel. Today, one task runs at a time through the same per-task steps.

## Requirements

On startup, Cronos-AI checks for required tools and prompts if any are missing; it does not install them automatically.

- A Rust toolchain to build the CLI and TUI.
- Pi agent to orchestrate workflow steps.
- Herdr to manage agents and their work. Tasks only run when Cronos-AI is inside a Herdr session.
- OpenSpec as single source of truth. Each project repository needs a committed `openspec/` directory (`openspec init`, then commit).
- A Git repository and a check command (for example `./scripts/check.sh`) for each project that will run tasks. Each attempt gets its own worktree.

## Build and run

```sh
cargo build --release
```

Create a project linked to a repository and add a task:

```sh
./target/release/cronos-ai project create demo --repository /path/to/repository
./target/release/cronos-ai project set-check-command <PROJECT_ID> -- ./scripts/check.sh
./target/release/cronos-ai task create <PROJECT_ID> "Build the feature" --priority high
```

The commands print JSON containing the project and task IDs. After building, start the dashboard with the launcher:

```sh
./scripts/cronos-ai
```

From a regular terminal, the launcher creates or attaches to the dedicated `cronos-ai` Herdr session, starts Cronos-AI in its own workspace, and opens Herdr. When run inside a Herdr pane, it starts the TUI directly in that pane. For a TUI in the current terminal without the Herdr launcher, run `./target/release/cronos-ai` directly; supervised task execution requires Herdr.

Select the project with **Enter**, press **s** on its task list to start the next ready task, then open the task to monitor progress. At the specification and final-code gates, press **v** to review the work and **y** or **n** to approve or request a revision. If the TUI was closed while a task ran, **s** resumes it.

You can also create projects and tasks directly in the dashboard:

1. On the project list, press **n**. Enter a project name and optionally a description, local Git repository path, and check command. Use **Tab**/**Shift+Tab** to move between fields, **Enter** to save, or **Esc** to cancel.
2. Select the project with **Enter**. On its task list, press **n** to create a task. Enter a title and optional description; priority defaults to **Medium**. Use **Tab**/**Shift+Tab** to move between fields, **←/→** to change priority, and **↑/↓** plus **Space** to select task dependencies.
3. Press **Enter** to save. The task remains **Pending**; press **s** when you are ready to start the next ready task.

The project form is only for new projects. To add a repository, check command, or check time limit to an existing project, use the CLI:

```sh
./target/release/cronos-ai project set-repository <PROJECT_ID> --repository /path/to/repository
./target/release/cronos-ai project set-check-command <PROJECT_ID> -- ./scripts/check.sh
./target/release/cronos-ai project set-check-timeout <PROJECT_ID> 90   # minutes, 1-480; omit to restore 60
```

## When something fails

Agents that crash, time out, or report failure stop the task with **Error**. Review, verification, and check-command findings instead send the code back to code generation automatically, up to 3 times per attempt. In an Error task's details:

| Key | Action |
|---|---|
| **t** | Retry only the failed step, keeping the attempt, worktree, and approvals. |
| **r** | Discard the attempt and start again from specification generation. |
| **c** | Like **r**, with extra context the new specifications must follow. |
| **a** | Abandon the task (Rejected) with a reason. |

A merge conflict sends the task back to code generation to resolve it. If the base repository has uncommitted changes, the task waits at local merge until you clean it and press **s**. See [workflow states](docs/workflow-states.md) for every outcome.

The data directory can be selected with `--data-dir DIR` or `CRONOS_AI_DATA_DIR`. Otherwise, Cronos-AI uses the platform's user data directory. See [storage format](docs/storage-format.md).

## Agent resource profiles

Every workflow agent loads a built-in skill for its step (see [`skills/`](skills/)). Profiles add Pi skills, extensions, MCP servers, a model, and a time limit on top. Manage them in the dashboard: press **p** on the project list to list, create, edit, and delete profiles. The editor checks every path before saving and shows which resources are missing.

![Agent profile editor in the Cronos-AI TUI](assets/agent-profiles.png)

You can also copy the example catalog into Cronos-AI's data directory and edit it by hand:

```sh
profile_dir="${CRONOS_AI_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/cronos-ai}"
mkdir -p "$profile_dir"
cp examples/agent-profiles.json "$profile_dir/agent-profiles.json"
```

Profiles have stable IDs and list the workflow steps where they can be selected. The orchestrator discovers eligible profiles for each step; workers load only their assigned profile resources. MCP profiles point to a separate Pi-compatible server config and an explicitly installed adapter. Cronos-AI does not install tools or run arbitrary profile shell hooks. See the [agent profile guide](docs/agent-profiles.md) and [sample catalog](examples/agent-profiles.json) for the format and details.

## Development

```sh
cargo test
./scripts/check.sh
```

The check script runs formatting, Clippy, all targets' tests, and `cargo audit`.
Run it automatically before every `git push` by installing the tracked hook once per clone:

```sh
mise install            # installs the pinned cargo-audit
mise run install-hooks  # points git at .githooks/
```

A failing check aborts the push; `git push --no-verify` skips it for a single push. End-to-end tests use temporary Git repositories with fake agents and a fake `herdr`:

```sh
cargo test --test workflow_e2e           # the full per-task workflow
cargo test --test herdr_agents_e2e       # Herdr agent lifecycle, cancellation, step lock
cargo test --test project_management_e2e # the CLI as a user drives it
```

The live smoke test starts one real Pi agent through Herdr. Run it by hand inside an isolated Herdr session:

```sh
CRONOS_LIVE_SMOKE=1 cargo test --test live_smoke -- --ignored --nocapture
```

Workers run with your operating-system permissions; see the [trust boundary](docs/integration-contracts.md#trust-boundary) before running untrusted work.

## Documentation

- [User guide](https://ixdi.github.io/cronos-ai/) (open in a browser)
- [Project/task commands and end-to-end use](docs/project-task-commands.md)
- [TUI monitoring and controls](docs/monitoring.md)
- [Workflow-agent profiles](docs/agent-profiles.md)
- [Workflow states and recovery](docs/workflow-states.md)
- [Storage format](docs/storage-format.md)
- [Pi, Herdr, and Git integration contracts](docs/integration-contracts.md)
- [System design](design/design.md)
- [Changelog](CHANGELOG.md)
