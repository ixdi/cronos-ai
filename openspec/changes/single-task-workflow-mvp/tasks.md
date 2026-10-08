# Tasks

## 1. Application scaffold and quality gates

- [x] 1.1 Create the Rust CLI/TUI application shell with a minimal Ratatui screen; verify `cargo check` and `cargo test` succeed.
- [x] 1.2 Configure automated formatting, lint, unit/integration/E2E test, and dependency-security checks; verify the full local quality command passes on the scaffold.

## 2. Project and task persistence

- [x] 2.1 Define project/task JSON models and atomic load/save behavior; add unit tests for persistence across reload, validation failures, and preserving invalid files, and document the format.
- [x] 2.2 Implement project create/view/update/select/archive and task creation with priority, dependencies, unique IDs, and initial Pending status; add tests for valid operations, persistence, and blocked dependencies, and document the CLI/TUI actions.

## 3. Tool integration boundary

- [x] 3.1 Verify the actual Pi JSON/RPC and Herdr API operations needed to launch agents, collect results, and cancel running agents; record the supported contracts in integration documentation.
- [x] 3.2 Implement the project-local Pi workflow extension, typed Rust task-state commands, and replaceable Herdr/OpenSpec/Git adapters with startup tool preflight; add fake-adapter tests for missing-tool prompts, successful calls, failures, and cancellation, and document the integration contracts.

## 4. Project dashboard and human controls

- [x] 4.1 Implement basic project/task TUI navigation and live task status, active step, and agent activity views; add UI-state tests and a short user guide for monitoring work.
- [x] 4.2 Implement human approval/rejection controls for specification and final-code gates, including revision-loop notes and terminal task abandonment; add tests that approval gates block progression and ordinary approval rejection leaves the task In Progress, and document the controls.

## 5. Workflow execution and recovery

- [ ] 5.1 Implement Pi-driven task orchestration with Rust queue/state support, ordered steps, priority/dependency selection, and parallel independent agents within each step; add tests proving step barriers, task serialization, and within-step concurrency, and document the workflow states.
- [ ] 5.2 Implement failure cancellation, Error status, retry from specification generation with downstream outputs discarded, and clarification-driven spec replacement/approval; add tests for sibling cancellation, ignored late results, Error-to-In Progress transitions, and recovery behavior, and document the recovery flow.
- [ ] 5.3 Implement per-task Git worktrees, documentation generation after final approval, and local merge without remote push; add tests for approval gating, successful local completion, merge conflicts, and absence of push calls, and document local completion and manual publishing.

## 6. End-to-end verification

- [ ] 6.1 Run the full workflow against temporary project data and fake external-tool adapters, covering project/task setup, approvals, parallel agents, failure recovery, documentation, and local merge; verify all required checks pass and update the user guide with the tested end-to-end flow.

## Workflow follow-up

- In a later change, create a Pi extension that integrates Jev for task prioritization; this is outside the first-release scope.
