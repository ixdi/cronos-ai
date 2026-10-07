# Spec Delta

## Purpose

Coordinate an automatically resolvable task through isolated implementation, review, verification, human rework or approval, and GitHub pull-request creation.

## ADDED Requirements

### Requirement: Process one task at a time
The system SHALL run no more than one task workflow at a time in the configured repository and base branch.

#### Scenario: Another task is queued while one is active
- **WHEN** a task workflow is active and another task is queued
- **THEN** the second task remains queued until the active workflow finishes or becomes blocked or errored

### Requirement: Isolate task execution
The system SHALL execute each task's changes in a workspace isolated from the configured repository's base worktree and from other task workspaces.

#### Scenario: Task changes stay in its workspace
- **WHEN** an agent edits files for a task
- **THEN** those edits are confined to that task's workspace until delivery

### Requirement: Route automatically resolvable work through implementation and verification
The system SHALL route work classified as automatically resolvable through implementation, code review, and verification before requesting final human review.

#### Scenario: Automatically resolvable task reaches human review
- **WHEN** triage classifies a task as automatically resolvable and the implementation and verification stages complete
- **THEN** the task's result and verification outcome are presented for human review

### Requirement: Block work requiring unsupported clarification or specifications
The system SHALL stop a task that requires clarification or specification approval, mark it blocked with the reason, and expose that status through the operator CLI rather than proceeding to implementation.

#### Scenario: Triage identifies work outside the MVP path
- **WHEN** triage determines that a task needs human clarification or a specification approval branch
- **THEN** the task becomes blocked, its reason is available through the CLI, and implementation does not start

### Requirement: Persist workflow progress for inspection
The system SHALL persist task and agent progress so that the operator CLI can inspect current and completed task state after a service restart.

#### Scenario: Operator inspects a task after restart
- **WHEN** the service restarts and an operator inspects a previously recorded task
- **THEN** the CLI reports its last persisted workflow and agent status

### Requirement: Inspect task workflow and review results
The operator CLI SHALL allow an operator to inspect task status, the current workflow stage, and available review or verification results.

#### Scenario: Inspect task awaiting human review
- **WHEN** an operator inspects a task awaiting review
- **THEN** the CLI displays its status, reviewable result, and verification outcome

### Requirement: Resume interrupted tasks only on operator request
The system SHALL mark tasks that were active when the service stopped as interrupted and SHALL offer an explicit CLI action to resume them from persisted workflow and workspace state.

#### Scenario: Active task is not resumed automatically
- **WHEN** the service restarts with a task that was active at shutdown
- **THEN** the task is marked interrupted and remains stopped until an operator requests resume

#### Scenario: Operator resumes an interrupted task
- **WHEN** an operator requests resume and the task checkpoint and workspace are available
- **THEN** the workflow continues from its latest persisted checkpoint using the task's existing workspace

#### Scenario: Interrupted task cannot be restored
- **WHEN** an operator requests resume but the task checkpoint or workspace is unavailable
- **THEN** the task remains interrupted and the CLI reports why resumption could not proceed

### Requirement: Rejection requests implementation rework
The system SHALL route a human rejection of final review back to implementation and repeat code review and verification before requesting another human decision.

#### Scenario: Human rejects a task for rework
- **WHEN** an operator rejects a task awaiting final review
- **THEN** the workflow returns to implementation and, after rework, runs code review and verification again before presenting it for review

### Requirement: Approval triggers documentation and pull-request creation
The system SHALL create documentation and then a GitHub pull request for the configured repository and base branch after final human approval.

#### Scenario: Approved task is delivered
- **WHEN** an operator approves a verified task
- **THEN** the documentation stage completes and the system creates a GitHub pull request for the configured repository and base branch

### Requirement: Stop workflow tracking after pull-request creation
The system SHALL mark a task delivered after its pull request is created and SHALL NOT track subsequent pull-request or CI status in this MVP.

#### Scenario: Pull request is created
- **WHEN** GitHub confirms creation of the task's pull request
- **THEN** the task is marked delivered and the workflow does not update it based on later pull-request or CI events
