# Design

## Context

See `proposal.md` for the motivation and MVP scope. The repository currently has no implementation or capability specs. `AGENTS.md` establishes Bun workspaces, SQLite persistence, LangGraph.js workflow orchestration, Docker/git-worktree task isolation, Herdr workspaces, and GitHub PR creation. The first slice handles one task at a time; the full TUI and parallel execution are deferred.

## Goals / Non-Goals

**Goals:**
- Keep workflow decisions and agent-role lifecycle in LangGraph, and keep durable task/workflow state in SQLite.
- Make the workspace provider replaceable without making Herdr concepts part of workflow logic.
- Keep agent execution independently replaceable behind a runner boundary.
- Provide HTTP task intake and a small CLI for human inspection, approval, and rejection.

**Non-Goals:**
- Implement parallel task scheduling, the full TUI, or post-PR/CI monitoring.
- Select a specific chat model for Pi coding roles; configure it at runtime. Jev is reserved for triage classification.
- Define detailed HTTP routes, CLI syntax, SQLite schema, or GitHub App implementation details.

## Decisions

### LangGraph owns workflow and agent lifecycle

LangGraph.js is the control plane for the task graph: it selects the next role, coordinates that role's execution, handles workflow transitions and approval gates, and records progress through the workflow state. Herdr supplies workspace/session capabilities; it is not the component that decides which agent runs or what role it has.

### Separate workspace provisioning from agent execution

Use two replaceable boundaries:

- **WorkspaceProvider** creates and closes an isolated task workspace and exposes the capabilities needed to work in it. Its first implementation uses Herdr, with a task-specific git worktree and Docker isolation as required by the project intent. The container mounts common Git metadata read-only and only the task's per-worktree metadata read-write, preventing agents from mutating shared refs; Git staging, commits, and PR operations remain host-orchestrated. It creates panes only when needed and closes them after their use. Workflow nodes should consume a provider-neutral workspace handle, not Herdr session or pane identifiers.
- **AgentRunner** executes a requested role against the task and workspace, reports progress/results to LangGraph, and supports lifecycle control. Pi is the initial CLI runner for coding roles and uses a separately configured chat model. Jev is used as the triage classifier, not as Pi's coding chat model. LangGraph coordinates the runner's role and lifecycle in the task's Herdr pane; workflow logic must not depend on Pi-specific or Herdr-specific APIs.

This is preferred to embedding Herdr operations or Pi-specific behavior in workflow nodes: either would make replacing the workspace provider or agent runner more invasive.

### SQLite is the persistence authority

Queue helpers provide queue-item save/read operations backed by SQLite. `cronos-storage` owns persistence for the queue and durable task/workflow/agent status; components use these operations rather than maintaining parallel JSON state. SQLite is also the source for restoring persisted application state after restart. This design does not prescribe table layout or transaction/claim semantics.

LangGraph checkpoints use a `BaseCheckpointSaver` implementation backed by Bun's built-in `bun:sqlite` and the same database file. Do not use the standalone LangGraph SQLite saver if it introduces `better-sqlite3`, which conflicts with the repository's Bun SQLite convention.

### Resume interrupted tasks only on operator request

On restart, a task that was active is marked interrupted and is not resumed automatically. The CLI offers an explicit resume action. Resuming restores the latest persisted workflow checkpoint and task workspace, then restarts the interrupted role under LangGraph control. If the checkpoint or workspace cannot be restored, the task remains interrupted and the CLI reports the problem.

### Keep intake and human interaction separate

Task submission enters through a loopback-only HTTP intake API and is persisted through queue helpers. The API is not exposed to the network in this slice; any future network exposure requires a separate authentication and rate-limiting design. A small CLI provides inspection, resume, and human approval/rejection; it is a temporary operator surface, not the full TUI. Both interact with the same SQLite-backed state and LangGraph-coordinated workflow.

### Constrain repository and delivery scope

Configure one target repository and base branch for the first slice. A task gets its own isolated worktree/runtime. After the required human approval and task completion, the workflow creates a GitHub PR. It then ends tracking for that task; merge status, CI results, and later PR events are outside this slice.

Authenticate PR creation as a GitHub App installed only on the configured repository. Grant only the repository permissions needed to create and push the task branch and open a pull request (contents write and pull requests write); do not grant workflow-write permission unless a later requirement needs it. Keep the App private key in deployment secret configuration, not SQLite or logs. Use short-lived installation access tokens for GitHub operations.

The initial `GitHubAppTokenProvider` uses `@octokit/auth-app`, reads App ID, installation ID, repository database ID, and private key from deployment environment settings, and requests a token for that single repository ID with exactly `contents: write` and `pull_requests: write`. It validates the returned repository scope, requested permissions (allowing only GitHub's implicit `metadata: read` permission if present), installation, and expiration; keeps tokens only in private in-memory state; and refreshes before expiry or after explicit invalidation. The deployment supplies credentials; no secret values are committed to the repository.

Before pushing, the host creates a temporary clone from the configured base branch with system/global Git configuration and hooks disabled, copies task files without copying agent-writable `.git` metadata, stages under the base repository's ignore rules, and commits in that host-owned clone. Git push uses an explicit `https://github.com/{owner}/{repository}.git` remote and passes the short-lived token through Git's per-process configuration environment, never command arguments or persisted config. Octokit creates the pull request only after the branch push succeeds.

## Risks / Trade-offs

- **A provider abstraction can become leaky** -> Keep Herdr-specific IDs and operations inside its adapter; pass provider-neutral workspace capabilities to workflow and agent execution.
- **The task workspace or checkpoint may be unavailable after restart** -> Keep the task interrupted, report the recovery problem, and do not silently start a fresh task workspace.
- **Pi/Herdr integration may expose provider-specific assumptions** -> Keep Pi process control behind `AgentRunner` and workspace operations behind `WorkspaceProvider`; verify the initial integration against those boundaries.
- **A single-repository configuration limits reuse** -> Accept this for the MVP; defer per-task repository selection and multi-repository permission handling.
- **Stopping at PR creation means failures after creation are not reflected in Cronos** -> Treat this as an explicit scope boundary; add PR/CI monitoring in a later change.
