# Tasks

## 1. Workspace foundation and source-of-truth documentation

- [x] 1.1 Create the Bun workspace and initial package scaffolding for `cronos-core`, `cronos-queue`, `cronos-runtime`, `cronos-storage`, and the minimal `cronos-panel` CLI; verify a clean `bun install` and workspace scripts succeed.
- [x] 1.2 Update `AGENTS.md` to say Jev model (not JDev) and clarify task-review rejection returns work to implementation; update `design/cronos_ai_workflow.mmd` to distinguish the MVP auto-resolvable path from deferred clarification/specification branches, on-demand panes, SQLite-only state, and stop-at-PR behavior; verify the docs agree with the approved specs and design.

## 2. SQLite persistence and queue

- [x] 2.1 Define and implement SQLite schema/migrations for queued tasks, workflow and agent status, and resumable workflow checkpoints; verify migrations work from an empty database and persistence tests pass.
- [x] 2.2 Implement queue save/read operations over the shared SQLite store; verify accepted tasks remain available after reopening the database and queue operations do not create duplicate task records.
- [x] 2.3 Integrate LangGraph checkpoint persistence with SQLite; verify a saved graph checkpoint can be loaded after a process restart and document the persistence boundaries.

## 3. Local task intake API

- [x] 3.1 Implement HTTP submission bound to the local host, validate required task content, and persist accepted requests through queue operations; verify valid submissions return a task identifier, invalid requests create no queue item, and the service is not reachable over a network interface.
- [x] 3.2 Add intake API usage and local-only deployment documentation; verify the documented request example succeeds and the documentation states that network exposure is out of scope.

## 4. Replaceable workspace and Pi runner

- [x] 4.1 Define provider-neutral `WorkspaceProvider` and `AgentRunner` contracts; verify contract tests with fakes cover create/use/close, progress/result reporting, and lifecycle cancellation without Herdr or Pi types leaking into workflow interfaces.
- [x] 4.2 Implement the Herdr workspace provider with a task-specific git worktree and Docker isolation, mounting common Git metadata read-only and only per-worktree metadata read-write; create panes only when needed and close them after use. Verify shared refs cannot be changed from the container, task files remain isolated, and pane lifecycle works with runtime integration tests.
- [x] 4.3 Implement the Pi CLI runner with a separately configured chat model for coding roles and Jev classifier support for triage, coordinated by LangGraph inside the task pane; verify a smoke task can read and change only its task workspace, report completion to LangGraph, and shut down cleanly.
- [x] 4.4 Document Herdr, Docker, git-worktree, Pi chat-model, Jev classifier, and required secret configuration; verify setup instructions work in a clean development environment and no real credentials are included.

## 5. LangGraph task workflow

- [x] 5.1 Implement the one-active-task workflow and triage routing for auto-resolvable work; mark tasks requiring clarification or specification approval blocked without starting implementation, and verify both routes with workflow tests.
- [x] 5.2 Implement the auto-resolvable path through implementation, code review, verification, final human review, and documentation; verify agent progress, results, and failures are persisted and covered by node-level tests.
- [x] 5.3 Implement restart reconciliation: mark an active task interrupted, do not resume automatically, and support explicit resume from the persisted checkpoint and existing workspace; verify successful resume and unavailable-checkpoint/workspace failure scenarios.
- [x] 5.4 Document workflow stages, task states, blocked-path behavior, and resume semantics; verify the workflow documentation matches executable graph transitions.

## 6. Operator CLI and human review

- [x] 6.1 Implement CLI inspection for queued, active, blocked, errored, interrupted, review-pending, and delivered tasks, including stage and available review/verification output; verify representative states render accurately in CLI tests.
- [x] 6.2 Implement explicit task resume and final approve/reject actions; verify reject returns to implementation and repeats review/verification, approval advances to documentation and delivery, and resume is never implicit.
- [x] 6.3 Document CLI commands and human review behavior; verify every documented command and example matches the CLI help and tests.

## 7. GitHub App and pull-request delivery

- [x] 7.1 Configure GitHub App credentials as deployment secrets, obtain repository-scoped short-lived installation tokens, and request only contents-write and pull-requests-write permissions; verify token refresh and that credentials never enter SQLite, logs, or responses.
- [ ] 7.2 Implement branch creation/push and GitHub pull-request creation for the configured repository and base branch; verify API behavior with mocked GitHub tests and a manual test against a disposable repository.
- [x] 7.3 Mark a task delivered after PR creation and stop handling subsequent PR/CI events; verify the stored delivered state includes the PR reference and does not change from later events.
- [x] 7.4 Document GitHub App registration, installation, repository permissions, and secret setup; verify the instructions require no broader permissions than the design specifies.

## 8. End-to-end integration verification

- [x] 8.1 Run an end-to-end test from local HTTP submission through SQLite queueing, isolated workspace execution, CLI human approval, documentation, and GitHub PR creation; verify the final task is delivered and no post-PR monitoring occurs.
- [x] 8.2 Run end-to-end rejection/rework, unsupported-task blocking, and restart/resume scenarios; verify task and agent state remains inspectable and no second task runs concurrently.
