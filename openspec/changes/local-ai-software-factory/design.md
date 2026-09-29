# Design

## Context

See `proposal.md` for motivation and scope. The repository currently contains a Python package scaffold and a documented workflow, but no implemented orchestrator, agents, or integrations. The factory is local-first, uses OpenSpec as the durable plan, LangGraph for live orchestration, Herdr for persistent worker terminals, and Pi as its initial agent runtime.

## Goals / Non-Goals

**Goals:**

- Make each accepted request auditable from plan through task execution, human decisions, verification, and delivery.
- Preserve a clear boundary between durable plans in the target repository and recoverable runtime state in the local controller.
- Allow independent implementation tasks to run concurrently without sharing writable worktrees or host privileges.
- Keep the factory useful through its CLI, with Herdr providing worker visibility rather than a custom approval UI.

**Non-Goals:**

- Provide a hosted or multi-user orchestration service.
- Replace OpenSpec, Herdr, Pi, or a target repository's CI/CD system.
- Automatically deploy outside the target repository's configured CI/CD workflow.
- Support arbitrary agent runtimes, monitoring vendors, or CI/CD vendors in the first implementation.

## Decisions

### Explicit setup and a persistent local controller

`factory init --repo <path>` explicitly prepares a target Git repository for OpenSpec planning. `factory run` requires an explicit target, an initialized OpenSpec root, and a clean checkout. It creates a run branch from the starting commit so the original branch is not modified during orchestration.

A single local controller process owns LangGraph scheduling, SQLite state, Herdr coordination, and webhook intake. The service holds a process lock so two controllers cannot dispatch the same task. CLI commands submit work and perform human actions through a local control interface. If the controller stops, SQLite retains queued and active task state; webhooks are unavailable until the controller is reachable again.

The controller creates and commits the OpenSpec plan on the run branch before implementation worktrees are created. This gives concurrent workers a shared, durable plan snapshot without implicitly committing to the user's original branch.

SQLite stores the run branch name, starting commit, run worktree path, task-worktree root, and a fingerprint of the approved OpenSpec plan so controller restarts and human actions can resolve the same resources safely.

OpenSpec records requirements, task decomposition, and dependencies; SQLite records live status, attempts, worker associations, and event cursors.

### Planning and approval

Every accepted request produces an OpenSpec change. Clear, bounded work uses concise artifacts and can proceed without a separate plan approval. Ambiguous or substantial work gets fuller specifications and waits for human approval. Security or authentication changes, destructive data migrations, and production infrastructure or deployment changes always require human approval before implementation.

Planning, review, and integration decisions are recorded as audited actions scoped to a run and task.

Plan approvals are bound to the stored plan fingerprint, so edits invalidate prior approval.

A clarification returns the request to triage.

Parked work is not dispatched.

### Dependency scheduling and recoverable state

LangGraph coordinates the execution graph, but SQLite is the source of truth for live task status and attempt history. Internal statuses are `queued`, `ready`, `running`, `waiting for human`, `blocked`, `review`, `done`, and `failed`. A task waiting only for dependencies remains `queued`; `blocked` means an external issue needs resolution.

Only dependency-ready tasks enter the worker scheduler, subject to a configured concurrency limit. Transient runtime failures receive bounded retries. Restart recovery reconciles persisted worker and session identifiers with Herdr before deciding whether to reconnect, retry, or surface a task for human attention.

### Herdr and Pi worker boundary

Herdr panes are reusable worker slots with one active task per slot. The controller uses the Herdr CLI for session and pane operations and the Socket API when a live event subscription is needed. A worker supervisor runs in the Herdr slot and manages a Pi RPC process, translating structured Pi events into factory progress and Herdr status metadata. Completion is based on Pi's settled event, not only prompt acceptance or terminal output.

Specialist profiles are factory-managed and explicitly list their role, prompt guidance, skills, MCP tools, model selection, provider API-key environment variable, and permitted network destinations.

Pi workers do not automatically load host or project skills, extensions, settings, or MCP servers.

The factory supplies a bundled Pi extension bridge for MCP servers because Pi RPC has no native MCP client.

The bridge starts only MCP server commands declared in factory-managed configuration, filters discovered tools against the profile's explicit tool allowlist, and never passes the provider API key to an MCP server process.

MCP server processes run inside the same task sandbox and use only its configured egress policy.

No default provider or model is assumed; each run must select a configured model and dedicated provider API key.

### Git isolation and integration

The run branch starts from the target repository's clean starting commit and contains the generated OpenSpec plan. Each concurrently executing implementation task receives its own Git worktree based on the latest run-branch integration point. A task becomes eligible for dependent work only after its changes have been verified and integrated.

The controller merges task branches automatically only when Git can do so cleanly. Conflicts preserve both worktrees and block integration for human resolution.

The run branch and worktree references are durable so conflict-resolution actions can target the correct checkout after restart.

Task execution never writes into the user's original checkout. Final delivery is left to the target repository's configured CI/CD and branch policy.

### Sandbox and credentials

Every implementation worker runs in a Docker-compatible sandbox, and the controller fails closed if isolation or required egress controls cannot be established. The sandbox exposes only the task worktree and specifically approved resources; it does not mount the host home directory or Pi authentication directory.

A dedicated provider API key is supplied for the worker process at launch, never persisted in SQLite, logs, or the repository. The key remains available to the Pi process and may be exposed to code executed in that container, so it must be narrowly scoped and must not grant access to unrelated services. OAuth credentials and reuse of the host Pi login are excluded from the first version.

Outbound network access is denied except for the configured model provider and explicitly approved package or task services. The exact provider and allowed package endpoints are run/profile configuration, not inferred from untrusted task content.

### Human control surface

Herdr's built-in agent sidebar shows worker lifecycle and concise task summaries. Factory CLI commands provide approval, clarification, retry, and conflict-resolution actions.

Each action is recorded with its run and task identifiers, actor, payload, timestamp, and outcome.

Detailed attention items include the reason, relevant attempt output or diff, and the available action.

Before delivery, the controller persists a review packet containing the exact integrated diff, code-review summary, automated verification results, and user-perspective scenarios.

Human approval is bound to the review and diff fingerprints; rejection returns integrated tasks to remediation.

The delivery gate rechecks the approved diff and verification evidence before allowing CI/CD dispatch.

The CLI is the authoritative action surface; no custom Herdr approval panel is required.

### Webhook and CI/CD integrations

Monitoring input uses a provider-neutral event schema. A user-managed HTTPS tunnel or otherwise reachable endpoint forwards signed events to the local controller; there is no hosted relay. The receiver validates schema, source authentication, timestamp, and event identity, then deduplicates events and submits normalized incident data to normal triage. Alert text and external attachments remain untrusted input and cannot override factory policy.

CI/CD is behind an explicit target-repository adapter contract. The configured adapter triggers the existing workflow and returns an authoritative status. Missing adapter configuration or an inconclusive result blocks delivery; the factory does not invent a successful result or implement its own deployment path. The initial implementation will provide the adapter boundary and test fake, with a concrete provider adapter selected when a target CI/CD system is known.

## Risks / Trade-offs

- [A model-directed command can still damage files or leak data available inside its container] → Limit mounts, isolate each task, allowlist egress, inject only a dedicated provider key, and require human review before CI/CD.
- [The provider API key is present in the worker container] → Use a dedicated, low-privilege key, never mount host credentials, prevent persistence in logs/state, and document rotation.
- [Local webhook delivery depends on the controller being reachable] → Require source retries or operator resubmission and show controller availability clearly; do not claim events were received while offline.
- [Parallel work may conflict even in separate worktrees] → Merge tasks in dependency order and stop for human resolution on conflicts.
- [A generic CI/CD contract cannot trigger every provider without an adapter] → Fail closed until the target repository has an explicitly configured supported adapter.
- [Container and network controls vary by host] → Verify isolation capability before worker launch and refuse a weaker unsandboxed fallback.

## Migration Plan

There is no existing runtime state or integration to migrate. Implement and validate the factory in thin end-to-end slices, beginning with repository initialization, local state, and a no-op worker path before enabling real agent execution. A rollback consists of stopping the controller and removing its local state; task worktrees and run branches remain inspectable and can be removed by the user after review.
