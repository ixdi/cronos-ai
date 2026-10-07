# Cronos operator CLI

The `cronos-panel` Bun package provides JSON CLI commands for inspecting queued and running tasks and for making explicit human workflow decisions. Durable state remains in the shared SQLite database; the CLI does not create JSON state files.

## Commands

```sh
# Help
bun run --filter cronos-panel start -- --help

# List all tasks or filter by one task status
bun run --filter cronos-panel start -- --db ./cronos.sqlite list
bun run --filter cronos-panel start -- --db ./cronos.sqlite list --status interrupted

# Inspect stage, workflow error, agent runs, and latest code-review/verification output
bun run --filter cronos-panel start -- --db ./cronos.sqlite show <task-id>

# Explicit recovery and human review actions
bun run --filter cronos-panel start -- --db ./cronos.sqlite resume <task-id>
bun run --filter cronos-panel start -- --db ./cronos.sqlite approve <task-id>
bun run --filter cronos-panel start -- --db ./cronos.sqlite reject <task-id> --feedback "Handle the empty input case"
```

`--db PATH` is optional. The database path defaults to `CRONOS_DB_PATH`, then `./cronos.sqlite`. Output is JSON. Exit code `0` indicates a successful query/action, `1` indicates a missing task or failed workflow action, and `2` indicates invalid command arguments.

`list` includes every task status (`queued`, `active`, `blocked`, `errored`, `interrupted`, `review_pending`, and `delivered`) and the persisted workflow stage when a workflow exists. `show` includes workflow error details and agent-run history; `codeReview` and `verification` expose their latest available summaries and structured gate results.

## Human review and restart behavior

- `list` and `show` are inspection-only: opening the database may apply schema migrations, but these commands do not change task state, reconcile active work, resume tasks, or start agents.
- `resume` is an explicit operator request for a task already marked `interrupted`. It requires a valid LangGraph SQLite checkpoint and matching persisted workspace. The runtime is recreated around the existing task worktree. If either resource is unavailable, the task remains interrupted and the CLI returns a reason. If the checkpoint is already at delivery, resume also requires the GitHub App settings and may create the PR.
- `approve` and `reject` are accepted only for `review_pending` tasks. Approval advances the saved workflow through documentation, then creates a task branch and GitHub PR when the GitHub App is configured. Rejection requires feedback, returns to implementation, and repeats code review and verification before asking for another decision.
- The workflow/agent actions can take as long as their Pi role runs. The CLI does not automatically resume a task after a process restart.
- The LangGraph graph sets the delivery stage to `pull_request`. The configured CLI delivery service creates the PR, then stores its number, URL, and branch and marks the task `delivered`. Without a pull-request creator, the core returns `ready_for_delivery` and leaves the task active. No PR/CI events are tracked after creation.

The core service must call `reconcileActiveTasks` during startup before claiming queued work. That operation marks previously active work interrupted and stops its runtime without resuming it. A task awaiting human review remains `review_pending` across restart.

## Workflow action configuration

`resume`, `approve`, and `reject` require these environment variables:

- `CRONOS_REPOSITORY_PATH`: host repository used to validate/restore the task worktree.
- `CRONOS_RUNTIME_ROOT`: host directory for isolated task worktrees and temporary host-owned delivery clones.
- `CRONOS_RUNTIME_IMAGE`: Docker image used to run task agents.
- `CRONOS_PI_CHAT_MODEL`: configured Pi chat model for coding, review, verification, and documentation roles. Triage uses Jev through Pi codemode instead.

`approve` and `resume` also require:

- `CRONOS_GITHUB_OWNER` and `CRONOS_GITHUB_REPOSITORY`: target repository path.
- `CRONOS_GITHUB_APP_ID`, `CRONOS_GITHUB_INSTALLATION_ID`, and `CRONOS_GITHUB_REPOSITORY_ID`.
- `CRONOS_GITHUB_APP_PRIVATE_KEY`: GitHub App PEM private key; escaped `\\n` newlines are accepted. Supply it through deployment secret configuration and never commit its value.

The App must be installed on the configured repository with only `contents: write` and `pull_requests: write` permissions. Tokens are installation-scoped to the configured repository ID, held in memory, and refreshed before expiry. Delivery stages files in a separate host-owned clone rather than trusting task-worktree Git metadata; push credentials are passed only to the Git subprocess and are not written to the worktree. See [`docs/github-app-setup.md`](../docs/github-app-setup.md) for registration, installation, permission, and deployment-secret instructions.

Optional settings:

- `CRONOS_BASE_BRANCH` (defaults to `main`).
- `CRONOS_RUNTIME_ENV`: comma-separated names of host environment variables allowlisted for forwarding into the task container. Supply names, not secret values; the runtime forwards the corresponding host values. GitHub App private-key and GitHub token variables are explicitly rejected from this allowlist.
- `HERDR_BINARY`, `DOCKER_BINARY`, `GIT_BINARY`, and `PI_BINARY` to select executable paths.

The panel package uses injected provider/runner dependencies in tests. In production the CLI constructs the Herdr workspace provider and Pi CLI runner from these environment settings.

## Tests

```sh
bun run --filter cronos-panel test
```
