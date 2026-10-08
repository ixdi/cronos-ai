# Spec Delta

## Purpose

Defines how Cronos-AI executes one task at a time through supervised workflow steps, including parallel agents within a step, approvals, and human-directed recovery.

## ADDED Requirements

### Requirement: Ordered single-task execution
The system SHALL execute no more than one task at a time in the first release and SHALL advance each task through its workflow steps in order, starting a step only after the preceding step is complete.

#### Scenario: Process the task queue
- **WHEN** multiple tasks are ready
- **THEN** the system processes one task at a time and does not start the next task until the current task is completed or explicitly abandoned

#### Scenario: Advance workflow steps
- **WHEN** a task step completes successfully
- **THEN** the next defined step starts

### Requirement: Parallel agents within a step
The system SHALL run independent sub-agents assigned to the same workflow step in parallel and SHALL wait for their results before advancing to the next step.

#### Scenario: Run independent agents
- **WHEN** multiple independent agents are assigned to one step
- **THEN** they run concurrently and the workflow waits for their results before advancing

### Requirement: Human approval gates
The system SHALL require human approval of task specifications before code generation and human approval of verified code before documentation and local integration. Rejecting an approval step SHALL start its revision loop without marking the entire task Rejected.

#### Scenario: Approve task specifications
- **WHEN** generated specifications are awaiting approval
- **THEN** code generation does not begin until a human approves them

#### Scenario: Clarify specifications
- **WHEN** a human supplies clarifying context for specifications
- **THEN** the system creates or replaces the specifications using that context and requires human approval before continuing to code generation

#### Scenario: Reject code at final approval
- **WHEN** a human rejects code at final approval
- **THEN** the workflow returns to code generation and the task remains In Progress

#### Scenario: Abandon a task
- **WHEN** a human explicitly abandons the entire task
- **THEN** the task is marked Rejected and is not processed further

### Requirement: Failure stops the workflow
If a sub-agent fails during a parallel step, the system SHALL cancel the other agents still running in that step, stop the workflow, and mark the task Error until a human chooses how to proceed.

#### Scenario: Agent fails in a parallel step
- **WHEN** a sub-agent fails while other agents in the same step are running
- **THEN** the other running agents are canceled, no later workflow step starts, and the task is marked Error

### Requirement: Human-directed task recovery
For an Error task, the system SHALL let a human retry from specification generation or clarify the context. A retry SHALL discard and not reuse outputs downstream of the specifications from the previous attempt. When processing resumes after the human's decision, the task status SHALL change from Error to In Progress.

#### Scenario: Retry after failure
- **WHEN** a human chooses to retry an Error task
- **THEN** the workflow restarts at specification generation and does not retain or reuse outputs downstream of the specifications from the previous attempt

#### Scenario: Clarify after failure
- **WHEN** a human clarifies the context for an Error task
- **THEN** the system creates or replaces the specifications, requires human approval, and continues the task workflow after approval

#### Scenario: Resume task status
- **WHEN** processing resumes after the human chooses retry or provides clarification
- **THEN** the task status changes from Error to In Progress

### Requirement: Validate work before final approval
The workflow SHALL run unit tests, end-to-end tests, security checks, and code-quality checks before requesting final human approval of the code.

#### Scenario: Verification gates final approval
- **WHEN** code generation and review have completed
- **THEN** the required tests and security and quality checks run before final code approval is requested

### Requirement: Complete locally without automatic push
After final approval and documentation, the system SHALL merge the task worktree locally and mark the task Completed. The first release SHALL NOT automatically push changes to a remote repository.

#### Scenario: Complete an approved task
- **WHEN** final approval, documentation, and local merge succeed
- **THEN** the task is marked Completed and no automatic remote push is performed
