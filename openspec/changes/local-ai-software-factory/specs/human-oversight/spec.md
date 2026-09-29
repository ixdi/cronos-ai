# Spec Delta

## Purpose

Keeps people in control of planning, high-impact work, code review, and exceptional recovery while making the factory's current work easy to inspect. It directs attention to tasks that need a human decision instead of routine dependency waits.

## ADDED Requirements

### Requirement: Require approval for detailed plans and high-impact changes
The factory SHALL hold implementation until a human approves plans requiring fuller specifications. Security or authentication changes, destructive data migrations, and production infrastructure or deployment changes SHALL require human approval even when triage considers their requirements clear.

#### Scenario: Approve a detailed plan
- **WHEN** a request requires specifications because of ambiguity or scope
- **THEN** implementation remains undispatched until a human approves the OpenSpec plan

#### Scenario: Identify high-impact work
- **WHEN** triage identifies a security or authentication change, destructive data migration, or production infrastructure or deployment change
- **THEN** the factory requires explicit human approval before implementation is dispatched

### Requirement: Obtain clarification before proceeding
The factory SHALL expose questions that block a request and SHALL wait for human clarification before revising the plan or dispatching implementation.

#### Scenario: Request needs clarification
- **WHEN** triage identifies a missing decision that affects expected behavior
- **THEN** the factory presents the question and keeps the task waiting for human input

#### Scenario: Human provides clarification
- **WHEN** a human answers the blocking question
- **THEN** the factory records the answer in the plan and returns the request to triage

### Requirement: Present actionable attention items
The factory SHALL surface tasks requiring human action with their current state, reason, relevant result or diff, and available next action. Tasks waiting only on dependencies SHALL NOT be presented as blocked human work.

#### Scenario: Task requires a human action
- **WHEN** a task is waiting for approval, clarification, failure recovery, or conflict resolution
- **THEN** it appears in the attention queue with the reason and an actionable next step

#### Scenario: Task waits on a dependency
- **WHEN** a task is queued only because its dependency is incomplete
- **THEN** it remains visible as queued progress but is not elevated as a human blocker

### Requirement: Require human review before CI/CD delivery
The factory SHALL present the integrated code changes and verification results for human review before triggering the target repository's CI/CD workflow. It SHALL not mark the run shipped before the configured delivery workflow succeeds.

#### Scenario: Approve reviewed work
- **WHEN** a human approves the integrated changes and verification results
- **THEN** the factory may trigger the configured CI/CD workflow

#### Scenario: Human requests changes
- **WHEN** a human rejects the review or requests changes
- **THEN** the factory returns the affected work to implementation or marks it blocked for clarification without triggering CI/CD

### Requirement: Provide explicit human recovery actions
The factory CLI SHALL let a human approve a plan or review, answer a clarification, retry an eligible failed task, and resolve a blocked integration. Each action SHALL be recorded against the run and task.

#### Scenario: Retry an eligible task
- **WHEN** a human requests a retry for a task marked failed
- **THEN** the factory records the action and retries only within the configured recovery policy

#### Scenario: Resolve an integration conflict
- **WHEN** a human resolves a blocked task integration
- **THEN** the factory records the resolution and resumes dependency-aware scheduling

### Requirement: Show concise status in Herdr
The factory SHALL report worker state and a concise task summary to Herdr. Herdr status presentation MAY use coarse `Working`, `Blocked`, and `Done` categories while the factory retains fine-grained task states and reasons.

#### Scenario: Inspect a worker in Herdr
- **WHEN** a human views a factory worker in Herdr
- **THEN** Herdr shows whether the worker is working, blocked, or done and identifies its current task
