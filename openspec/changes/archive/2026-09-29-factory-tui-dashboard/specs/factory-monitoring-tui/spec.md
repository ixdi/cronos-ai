# Spec Delta

## Purpose

Provides operators with a live, read-only terminal view of concurrent Cronos AI factory runs, their workflow progress, worker activity, human attention, and generated results. It makes durable execution state and safe progress history understandable without requiring direct database access or repeated CLI queries.

## ADDED Requirements

### Requirement: Open a live factory dashboard
The application SHALL provide an interactive terminal dashboard that reads the configured factory state and refreshes to show current controller and run information. It SHALL preserve existing non-interactive CLI command behavior and SHALL report a clear, actionable error when interactive terminal operation is unavailable.

#### Scenario: Open dashboard with persisted runs
- **WHEN** an operator starts the dashboard with a valid state directory containing factory runs
- **THEN** the dashboard displays the controller's current availability and an overview of the persisted runs, and refreshes the displayed state while open

#### Scenario: Open dashboard with no runs
- **WHEN** an operator starts the dashboard and no runs have been persisted
- **THEN** the dashboard shows an explicit empty state and explains how to submit work

#### Scenario: Dashboard cannot use the terminal
- **WHEN** an operator starts the dashboard without an interactive terminal
- **THEN** the command exits without corrupting factory state and reports that an interactive terminal is required

### Requirement: Summarize factory and task progress
The dashboard SHALL list each persisted run with its identifier, request summary, current aggregate state, current workflow stage, and task counts by lifecycle state. It SHALL identify active worker assignments and indicate runs awaiting human action, blocked, failed, or complete. Any displayed completion percentage SHALL be computed from completed planned tasks divided by total planned tasks, identify that basis to the operator, and avoid presenting an ungrounded time-to-completion estimate.

#### Scenario: Inspect concurrent runs
- **WHEN** multiple runs are persisted, including active and completed work
- **THEN** the overview displays them separately with their respective stage, task-state counts, worker assignments, and completion percentages

#### Scenario: No task has completed
- **WHEN** a run has planned tasks and none has reached the completed state
- **THEN** the dashboard displays zero percent completed and the total task count used for that calculation

#### Scenario: Tasks have completed
- **WHEN** some tasks in a run are complete and others are incomplete
- **THEN** the displayed percentage equals the completed-task count divided by the planned-task count, rounded consistently, and does not imply that remaining work is time-estimated

### Requirement: Inspect run and task details
The dashboard SHALL let an operator select a run and inspect its task descriptions and states, workflow plan, assigned worker, attempt history, latest errors or block reasons, human-attention items, and available review summary. It SHALL identify unavailable or missing details rather than implying that information exists when it does not.

#### Scenario: Inspect a blocked task
- **WHEN** an operator opens a task whose state is blocked or failed
- **THEN** the task detail includes its available reason and attempt history and identifies any applicable human-attention action without executing that action

#### Scenario: Inspect a run without review evidence
- **WHEN** an operator opens a run for which no review packet exists
- **THEN** the dashboard reports that review evidence is not yet available

### Requirement: Persist and display safe activity history
The application SHALL make useful progress and lifecycle activity for runs and tasks available in the dashboard across controller restarts, including timestamps and associated run/task identities where known. Activity history SHALL be bounded or navigable without requiring the dashboard to load an unbounded log into memory. The application SHALL avoid displaying or persisting provider credentials, environment secrets, or raw tool payloads as activity content.

#### Scenario: Review progress after a restart
- **WHEN** a worker reports progress and the controller is later restarted
- **THEN** the operator can inspect the persisted progress and lifecycle events for the associated run and task

#### Scenario: Display a failed attempt
- **WHEN** a task attempt fails or is interrupted
- **THEN** its event history displays the timestamp, outcome, and available safe failure summary

#### Scenario: Activity contains sensitive content
- **WHEN** worker progress or failure data contains a credential, secret, or raw tool payload
- **THEN** the activity history does not expose that sensitive value or unfiltered payload in the dashboard

### Requirement: Inspect generated outputs safely
The dashboard SHALL identify available generated artifacts and outputs associated with a run, including its OpenSpec change location, run worktree location, and review summary or evidence when available. It SHALL distinguish a recorded path or summary from the underlying file contents and SHALL report when a referenced output is unavailable.

#### Scenario: Run outputs are available
- **WHEN** an operator opens a run with persisted plan and worktree references
- **THEN** the dashboard shows those references and available review outputs for that run

#### Scenario: A recorded output is unavailable
- **WHEN** the dashboard cannot access a referenced output
- **THEN** it identifies the output as unavailable and does not fail the rest of the dashboard

### Requirement: Keep dashboard monitoring read-only
The dashboard SHALL NOT approve plans or reviews, retry tasks, resolve conflicts, enqueue actions, or otherwise mutate run lifecycle state in response to viewing or navigating its screens. Operators SHALL continue to use the existing command interfaces for those state-changing actions.

#### Scenario: Navigate attention items
- **WHEN** an operator views an item requiring human attention or chooses to inspect its suggested next action
- **THEN** the dashboard displays the decision context but does not execute or enqueue the action

#### Scenario: Refresh dashboard data
- **WHEN** the dashboard refreshes its view
- **THEN** it reads updated state without changing requests, tasks, attempts, approvals, or control actions
