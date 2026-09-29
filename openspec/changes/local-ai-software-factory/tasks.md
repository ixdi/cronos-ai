# Tasks

## 1. Foundation and Contracts

- [x] 1.1 Add the minimal Python package, test, lint, and type-check setup required by the factory; verify a clean install, CLI entry point, and baseline checks run successfully.
- [x] 1.2 Define validated configuration and domain models for requests, OpenSpec plans, task states, specialist profiles, worker slots, attempts, and integration results; write failing unit tests first and verify invalid states and configuration are rejected.
- [x] 1.3 Implement SQLite schema creation, versioned migrations, transactional task and attempt persistence, and single-controller locking; write tests first and verify state survives closing and reopening the database.

## 2. Repository Setup and OpenSpec Planning

- [x] 2.1 Implement `factory init --repo <path>` with Git-root validation and explicit OpenSpec setup; write tests first and verify it initializes only the selected repository.
- [x] 2.2 Implement request intake requiring an explicit repository path, initialized OpenSpec, and a clean working tree; write tests first and verify invalid or dirty targets are rejected without modifying them.
- [x] 2.3 Implement triage outcomes for actionable, specifications-required, clarification-required, and parked work; write tests first and verify specifications-required requests can advance to detailed planning but not implementation dispatch until approved, while only actionable requests can dispatch directly.
- [x] 2.4 Generate a concise OpenSpec change for every accepted request and fuller artifacts when scope requires them; write tests first and verify generated artifacts pass `openspec validate` in a temporary target repository.
- [x] 2.5 Add high-impact and plan-approval gates for ambiguous or substantial work, security or authentication changes, destructive migrations, and production infrastructure or deployment; write tests first and verify implementation stays undispatched until the required approval is recorded.
- [x] 2.6 Document repository initialization, request intake, and planning behavior; verify every documented command and prerequisite against the CLI.

## 3. Run Branches and Local Controller

- [x] 3.1 Create an isolated run branch from the clean starting commit, place and commit the OpenSpec plan there, and preserve the user's original branch; write Git integration tests first and verify the original branch and checkout remain unchanged.
- [x] 3.2 Implement the long-running local controller and CLI control interface for submitting work, inspecting status, and issuing human actions; write tests first and verify a second controller cannot acquire the active-controller lock.
- [x] 3.3 Connect LangGraph transitions to the SQLite execution ledger and dependency graph; write tests first and verify task state and attempt history remain consistent across controller restart.
- [x] 3.4 Document controller startup, shutdown, local state location, and restart behavior; verify a documented restart reconnects to persisted state without creating duplicate runs.

## 4. Git Worktrees and Sandbox Boundary

- [x] 4.1 Create and clean up one Git worktree per active implementation task from the run branch; write tests first and verify concurrent worktrees have distinct paths and the original checkout is untouched.
- [x] 4.2 Implement a Docker-compatible sandbox adapter that mounts only the task worktree, fails closed without isolation, injects only the configured provider API key, and enforces configured egress destinations; write tests first and verify mounts, environment, and denied destinations.
- [x] 4.3 Integrate clean task branches into the run branch in dependency order and surface merge conflicts as blocked work without overwriting either result; write Git tests first and verify clean merges and conflicting merges separately.
- [x] 4.4 Document sandbox prerequisites, API-key handling, egress configuration, and worktree recovery; verify the guide contains no host credential or secret values.

## 5. Herdr and Pi Worker Runtime

- [x] 5.1 Implement the Herdr adapter for session and pane lifecycle, structured worker status, and reconnectable worker identifiers; write tests against a fake Herdr CLI and Socket API first and verify restart reconciliation reuses live workers.
- [x] 5.2 Implement a Pi RPC worker supervisor that sends prompts, consumes JSONL events, and reports completion only after `agent_settled`; write protocol tests first and verify malformed records, process exits, and accepted-but-unsettled prompts are handled safely.
- [x] 5.3 Implement factory-managed specialist profiles with explicit skills, MCP tools, model selection, provider API key configuration, and network allowlists; write tests first and verify unlisted host or project resources are not enabled.
- [x] 5.4 Run one implementation task end-to-end in a Herdr slot, Pi RPC process, task worktree, and sandbox; verify the worker reports progress, produces an inspectable diff, and never accesses the host home directory.
- [x] 5.5 Document Pi setup, approved profile resources, provider configuration, and Herdr integration; verify setup instructions against the supported installed versions.

## 6. Scheduling, Recovery, and Human Oversight

- [x] 6.1 Implement bounded-concurrency scheduling that dispatches only dependency-ready tasks to reusable worker slots; write tests first and verify dependency waits remain queued and each slot runs at most one task.
- [x] 6.2 Implement fine-grained task transitions, reason-bearing blocked and failed states, bounded transient retries, and controller restart recovery; write tests first and verify attempts never exceed the configured limit and surviving workers are not duplicated.
- [x] 6.3 Implement the human attention queue and run-scoped CLI actions for approval, clarification, retry, and conflict resolution with an audit record; persist the run branch, worktree, and plan fingerprint references needed for safe restart recovery; write tests first and verify dependency-only waits do not appear as human blockers.
- [x] 6.4 Map worker state and concise task summaries into Herdr's sidebar metadata; write tests against the Herdr adapter first and verify fine-grained factory states map to truthful coarse display states.
- [x] 6.5 Implement fingerprinted review packets containing the integrated diff, code-review summary, automated checks, and user-perspective results; gate delivery on explicit human approval and current evidence; write tests first and verify rejection returns work to remediation and blocks delivery.
- [x] 6.6 Document task states, queue behavior, approvals, retries, and review expectations; verify each documented state and action matches the implementation.

## 7. Monitoring and CI/CD Integrations

- [x] 7.1 Implement the provider-neutral webhook endpoint with source authentication, schema validation, timestamp/replay protection, and event deduplication; write security tests first and verify invalid or repeated events create no duplicate task.
- [x] 7.2 Normalize accepted alerts as untrusted triage input and apply ordinary planning and high-impact gates; write tests first and verify alert content cannot bypass approval or execute commands directly.
- [x] 7.3 Implement the CI/CD adapter contract and a deterministic fake adapter for dispatch, status, and failure; write tests first and verify missing configuration or inconclusive results never report successful delivery.
- [x] 7.4 Document secure webhook reachability, source configuration, deduplication behavior, and CI/CD adapter setup; verify the guide clearly states that no hosted relay or default provider is supplied.

## 8. End-to-End Acceptance

- [x] 8.1 Add an end-to-end test using fake Pi, Herdr, sandbox, webhook, and CI/CD adapters to exercise request intake, OpenSpec planning, approvals, dependency scheduling, worktree integration, restart recovery, and successful delivery; verify the run reaches `done` only after every required gate succeeds.
- [x] 8.2 Add failure-path end-to-end tests for clarification, high-impact approval, exhausted retries, worker restart, merge conflicts, invalid webhooks, and CI/CD failure; verify each path stops safely and exposes the expected human action.
- [x] 8.3 Run the full test, lint, type-check, security, and packaging checks; document exact commands and verify a clean environment can install and run the factory CLI.
