# task-workflow Specification

## Purpose

Defines how Cronos-AI executes one task at a time through supervised workflow steps, including parallel agents within a step, approvals, and human-directed recovery.

## Requirements

### Requirement: Ordered single-task execution
The system SHALL execute no more than one task at a time in the first release and SHALL advance each task through its workflow steps in order, starting a step only after the preceding step is complete.

#### Scenario: Process the task queue
- **WHEN** multiple tasks are ready
- **THEN** the system processes one task at a time and does not start the next task until the current task is completed or explicitly abandoned

#### Scenario: Advance workflow steps
- **WHEN** a task step completes successfully
- **THEN** the next defined step starts

### Requirement: Parallel agents within a step
The system SHALL run independent sub-agents assigned to the same workflow step in parallel and SHALL wait for their results before advancing to the next step. Specification and code generation SHALL use exactly one agent; code review, verification, and documentation SHALL accept one to four agents. Code review and verification agents SHALL NOT modify the task worktree.

#### Scenario: Run independent agents
- **WHEN** multiple independent agents are assigned to one step
- **THEN** they run concurrently and the workflow waits for their results before advancing

#### Scenario: Reject parallel writers
- **WHEN** an agent plan assigns more than one agent to specification or code generation
- **THEN** the system rejects the plan before starting any agent

#### Scenario: Read-only step modifies the worktree
- **WHEN** a code review or verification step finishes and the worktree's commit or file status changed
- **THEN** the step fails and the task is marked Error

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
If a sub-agent fails during a parallel step, the system SHALL cancel the other agents still running in that step, stop the workflow, and mark the task Error until a human chooses how to proceed. An agent fails when it crashes, exceeds its time limit, blocks on a prompt, or leaves a missing, invalid, or `failed` result.

#### Scenario: Agent fails in a parallel step
- **WHEN** a sub-agent fails while other agents in the same step are running
- **THEN** the other running agents are canceled, no later workflow step starts, and the task is marked Error

#### Scenario: Agent leaves no valid result
- **WHEN** an agent settles without writing a valid structured result
- **THEN** the agent is treated as failed

#### Scenario: Sibling has not started yet
- **WHEN** a sibling agent's cancellation is requested before its launch finished
- **THEN** that agent is not started or prompted afterwards, and the step returns Error without waiting for it to run

#### Scenario: Agent panes are cleaned up
- **WHEN** an agent finishes, fails, or is cancelled
- **THEN** the Herdr pane opened for it is closed

### Requirement: Human-directed task recovery
For an Error task, the system SHALL let a human retry from specification generation, clarify the context, or retry only the failed step. A retry from specification generation SHALL discard and not reuse outputs downstream of the specifications from the previous attempt. Retrying the failed step SHALL keep the attempt, worktree, and approvals. When processing resumes after the human's decision, the task status SHALL change from Error to In Progress.

#### Scenario: Retry after failure
- **WHEN** a human chooses to retry an Error task
- **THEN** the workflow restarts at specification generation and does not retain or reuse outputs downstream of the specifications from the previous attempt

#### Scenario: Clarify after failure
- **WHEN** a human clarifies the context for an Error task
- **THEN** the system creates or replaces the specifications, requires human approval, and continues the task workflow after approval

#### Scenario: Retry the failed step
- **WHEN** a human retries only the failed step of an Error task
- **THEN** the task resumes at that step with the same attempt, worktree, and approvals

#### Scenario: Resume task status
- **WHEN** processing resumes after the human chooses retry, step retry, or provides clarification
- **THEN** the task status changes from Error to In Progress

### Requirement: Validate work before final approval
The workflow SHALL run the project's configured check command in the task worktree after the verification agents succeed and before requesting final human approval. A non-zero exit SHALL return the task to code generation as an automatic revision, recording the end of the command output.

#### Scenario: Verification gates final approval
- **WHEN** code generation and review have completed
- **THEN** the verification agents and the project check command run before final code approval is requested

#### Scenario: Check command fails
- **WHEN** the project check command exits unsuccessfully
- **THEN** the task returns to code generation in progress with the command output recorded

#### Scenario: Check command is missing
- **WHEN** verification runs for a project without a check command
- **THEN** the step fails and the task is marked Error

#### Scenario: Check command leaves files behind
- **WHEN** the check command succeeds but leaves new or modified files in the worktree
- **THEN** the task returns to code generation with the files listed in the findings

#### Scenario: Task branch changes the check program
- **WHEN** the check program is a file in the repository and the task branch changed it
- **THEN** a warning is recorded with the check result and shown in the review view

#### Scenario: Check command times out
- **WHEN** the check command exceeds its limit
- **THEN** it and every process it started are stopped and the check counts as failed

### Requirement: Complete locally without automatic push
After final approval and documentation, the system SHALL merge the task worktree locally and mark the task Completed. The first release SHALL NOT automatically push changes to a remote repository. A dirty base repository SHALL leave the task waiting at local merge; a merge conflict SHALL be aborted and return the task to code generation.

#### Scenario: Complete an approved task
- **WHEN** final approval, documentation, and local merge succeed
- **THEN** the task is marked Completed and no automatic remote push is performed

#### Scenario: Base repository is dirty
- **WHEN** the base repository has uncommitted changes at local merge
- **THEN** the task stays In Progress at local merge and the user can retry after cleaning the base

#### Scenario: Merge conflicts
- **WHEN** the local merge conflicts
- **THEN** the merge is aborted, the base is unchanged, and the task returns to code generation to reconcile with the base branch

#### Scenario: Merge fails without a conflict
- **WHEN** the local merge fails and leaves no unmerged paths
- **THEN** the merge is aborted, the base is unchanged, the failure is recorded, and the task is marked Error

### Requirement: Structured agent results
Each workflow agent SHALL write a JSON result in the task worktree with a status of `success`, `changes_requested`, or `failed`, a nonblank summary, and optional findings. The system SHALL decide step outcomes from these results, not from terminal output, and SHALL keep result files out of Git.

#### Scenario: Agent reports success
- **WHEN** every agent in a step writes a valid `success` result
- **THEN** the step advances and each summary is recorded in agent activity

#### Scenario: Result files stay untracked
- **WHEN** agents write result files in the worktree
- **THEN** the files do not make the worktree dirty and are never committed

### Requirement: Automatic revision loop
When a code review or verification agent reports `changes_requested`, the system SHALL wait for the step's agents, record the findings, and return the task to code generation in progress. It SHALL allow at most three automatic revisions per attempt and then mark the task Error.

#### Scenario: Reviewer requests changes
- **WHEN** a code review agent reports `changes_requested`
- **THEN** the findings are recorded and the task returns to code generation in progress

#### Scenario: Revision limit reached
- **WHEN** a fourth automatic revision would start in the same attempt
- **THEN** the task is marked Error and the reason is recorded in agent activity

### Requirement: Per-step agent time limits
Each agent SHALL run under a time limit: 30 minutes for specification generation, code review, and documentation, and 60 minutes for code generation and verification, unless its profile sets `timeout_minutes`. An agent that exceeds its limit SHALL be canceled and treated as failed.

#### Scenario: Agent exceeds its limit
- **WHEN** an agent does not settle within its time limit
- **THEN** it is canceled and the step fails

### Requirement: Resume interrupted tasks
The system SHALL let a human resume an In Progress task that has no running orchestrator and is not waiting for approval. Resuming SHALL mark unfinished agents of the current step as cancelled and either retry the local merge or restart orchestration at the current step.

#### Scenario: Resume after the TUI closed
- **WHEN** a human resumes a task whose orchestrator stopped during an agent step
- **THEN** unfinished agents are marked cancelled and orchestration restarts at the same step

### Requirement: OpenSpec change per task
Each task SHALL use one OpenSpec change named `cronos-` followed by the first eight hex characters of its ID. Starting work SHALL require a committed `openspec/` directory in the repository. The specification step SHALL leave a strictly valid change. Documentation SHALL archive the change, syncing its specs before the local merge.

#### Scenario: Repository lacks OpenSpec
- **WHEN** a task starts in a repository without a committed `openspec/` directory
- **THEN** no worktree is kept and the user is told to initialize and commit OpenSpec

#### Scenario: Invalid specification
- **WHEN** specification generation finishes and the task's change fails strict validation
- **THEN** the step fails and the task is marked Error

#### Scenario: Archive during documentation
- **WHEN** the documentation agents succeed
- **THEN** the task's change is archived and the documentation is committed before the local merge

### Requirement: Committed writing steps
The specification and code generation agents SHALL commit their changes with Conventional Commit messages, and the step SHALL fail if the worktree is left dirty. Documentation agents SHALL NOT commit; the system SHALL make a single documentation commit after archiving the change.

#### Scenario: Writer leaves uncommitted changes
- **WHEN** a code generation agent succeeds but leaves the worktree dirty
- **THEN** the step fails and the task is marked Error

### Requirement: Orchestrator context and agent contract
The orchestrator SHALL have read-only access to the task worktree and only the Cronos workflow tools. The system SHALL append a fixed contract to every agent prompt that states the result file, the OpenSpec change, the step's write rules, and that agents must not push or edit Cronos state.

#### Scenario: Agent receives the contract
- **WHEN** an agent is launched for a step
- **THEN** its prompt ends with the system-defined contract for that step

### Requirement: Relocatable orchestrator extension
The workflow extension SHALL be embedded in the Cronos-AI binary and written to the data directory before the orchestrator starts, so the binary does not depend on its source checkout.

#### Scenario: Run a moved binary
- **WHEN** the release binary runs outside its build directory
- **THEN** the orchestrator loads the extension from the data directory

### Requirement: Documented worker trust boundary
Documentation SHALL state that workers run with the user's operating-system permissions, that Cronos tools cannot change status or approvals but a worker's shell could, and SHALL recommend a dedicated user or container for untrusted work.

#### Scenario: Read the trust model
- **WHEN** a user reads the integration documentation
- **THEN** it describes what the gates protect against and what requires operating-system isolation

### Requirement: One step process per task
The system SHALL let only one process run a workflow step for a task at a time, using an operating-system lock that is released when the process exits. Resume, step retry, retry, and clarification SHALL refuse to act while the lock is held, and SHALL stop agents left by an earlier process before continuing. A step SHALL discard its results if the task's attempt, step, or status changed while its agents ran. Herdr agent names SHALL include the attempt.

#### Scenario: Recovery while a step still runs
- **WHEN** a human retries or resumes a task while another process is running one of its steps
- **THEN** the action is refused with a message and nothing changes

#### Scenario: Recovery after a crashed orchestrator
- **WHEN** a human retries or resumes a task whose step process exited while agents were running
- **THEN** those agents are stopped and their panes closed before the task continues

#### Scenario: New attempt
- **WHEN** a task starts a new attempt
- **THEN** its agents use Herdr names different from every earlier attempt's agents
