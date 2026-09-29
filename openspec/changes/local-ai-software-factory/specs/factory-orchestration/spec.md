# Spec Delta

## Purpose

Provides the local control plane that turns accepted requests into durable plans and schedules their dependency-ready work. It preserves execution state across controller restarts so the factory can resume without losing or duplicating work.

## ADDED Requirements

### Requirement: Explicit repository initialization and task target
The factory SHALL provide an explicit initialization action for a selected Git repository. A task run SHALL require an explicit repository path, an initialized OpenSpec root, and a clean working tree, and SHALL reject an invalid target before changing it.

#### Scenario: Initialize a repository for factory use
- **WHEN** a user explicitly initializes a valid Git repository
- **THEN** the factory sets up the OpenSpec root needed for future plans

#### Scenario: Reject a task run with an unsafe or uninitialized target
- **WHEN** a user starts a task against a non-Git repository, an uninitialized repository, or a repository with uncommitted changes
- **THEN** the factory reports the unmet prerequisite and makes no changes to the target repository

### Requirement: Durable OpenSpec plan for accepted requests
The factory SHALL create an OpenSpec change for every accepted request and SHALL use its tasks and dependencies as the durable execution plan. Clear, bounded work MAY use concise planning artifacts, while ambiguous or substantial work SHALL receive a fuller plan before implementation is dispatched.

#### Scenario: Plan a clear bounded request
- **WHEN** triage classifies a request as clear and bounded
- **THEN** the factory creates a concise OpenSpec plan and makes its dependency-ready tasks eligible for scheduling

#### Scenario: Plan substantial or ambiguous work
- **WHEN** triage determines that scope or requirements need clarification or fuller specifications
- **THEN** the factory records the detailed plan in OpenSpec and holds implementation tasks for the required human approval

### Requirement: Route requests through triage
The factory SHALL classify each request as actionable, requiring specifications, requiring human clarification, or parked. It SHALL NOT dispatch work that is parked or awaiting clarification.

#### Scenario: Clarification is required
- **WHEN** triage cannot determine the intended outcome from the request
- **THEN** the request waits for human clarification and is not dispatched

#### Scenario: Request is parked
- **WHEN** triage determines that a request should be parked
- **THEN** the request remains undispatched until a human resumes it

### Requirement: Dependency-aware task scheduling
The factory SHALL dispatch only tasks whose declared dependencies are complete. Tasks waiting only for dependencies SHALL remain queued rather than being presented as human-blocked work.

#### Scenario: Schedule ready tasks
- **WHEN** a task has no incomplete dependencies and an execution slot is available
- **THEN** the scheduler makes that task eligible for dispatch

#### Scenario: Hold a dependent task
- **WHEN** a task depends on another task that is not complete
- **THEN** the scheduler keeps it queued and does not assign it to a worker

### Requirement: Durable task state and restart recovery
The factory SHALL persist run and task execution state, including the task state, attempt history, assigned worker, and references needed to reconnect to active Herdr sessions. After controller restart, it SHALL reconcile persisted work with Herdr before dispatching replacements and SHALL avoid duplicate active execution.

#### Scenario: Recover an active worker after restart
- **WHEN** the controller restarts while a Herdr worker is still active
- **THEN** the factory reconnects the persisted task to that worker instead of launching a duplicate

#### Scenario: Resume work when no worker survived
- **WHEN** the controller restarts and a persisted running task has no active worker
- **THEN** the factory records the interrupted attempt and applies the configured recovery policy before redispatching it

### Requirement: Track fine-grained task states
The factory SHALL track tasks as `queued`, `ready`, `running`, `waiting for human`, `blocked`, `review`, `done`, or `failed`. It SHALL preserve the reason and relevant attempt details when a task becomes blocked or failed.

#### Scenario: Record progress through execution
- **WHEN** a task becomes eligible, starts, enters review, or completes
- **THEN** its persisted state reflects the corresponding lifecycle stage

#### Scenario: Explain a blocked or failed task
- **WHEN** a task becomes blocked or failed
- **THEN** the factory records a human-readable reason and the task's latest attempt information

### Requirement: Bounded recovery from transient failures
The factory SHALL retry transient worker or runtime failures only up to a configured finite limit. It SHALL preserve each attempt and SHALL mark work failed for human attention when a non-transient failure occurs or the retry limit is exhausted.

#### Scenario: Recover from a transient failure
- **WHEN** a worker encounters a failure classified as transient and retry capacity remains
- **THEN** the factory records the failed attempt and retries the task within the configured limit

#### Scenario: Stop after permanent or exhausted failure
- **WHEN** a failure is non-transient or the retry limit is exhausted
- **THEN** the factory marks the task failed and exposes it for human attention without retrying indefinitely

### Requirement: Operate through a persistent local controller
The factory SHALL provide a long-running local controller that owns scheduling and webhook intake while active. CLI requests SHALL be recorded durably, and stopping the controller SHALL preserve queued work while making clear that webhooks cannot be received until the controller is reachable again.

#### Scenario: Submit work while the controller is active
- **WHEN** a user submits a valid request to the local controller
- **THEN** the factory records the request and schedules it according to its plan and dependencies

#### Scenario: Restart the local controller
- **WHEN** the controller is stopped and later restarted
- **THEN** it recovers persisted tasks and worker associations before resuming scheduling
