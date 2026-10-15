# task-workflow Specification

## Purpose

Defines how Cronos-AI executes tasks, one at a time or in parallel up to a project capacity, through supervised workflow steps, including parallel agents within a step, approvals, and human-directed recovery.

## Requirements

### Requirement: Parallel agents within a step
The system SHALL run independent sub-agents assigned to the same workflow step in parallel and SHALL wait for their results before advancing to the next step. Specification generation SHALL use exactly one agent; code generation, code review, verification, and documentation SHALL accept one to four agents. Parallel code-generation agents SHALL each work in a separate worktree, and their branches SHALL be merged into the task branch in agent-name order. Code review and verification agents SHALL NOT modify the task worktree.

#### Scenario: Run independent agents
- **WHEN** multiple independent agents are assigned to one step
- **THEN** they run concurrently and the workflow waits for their results before advancing

#### Scenario: Reject parallel writers
- **WHEN** an agent plan assigns more than one agent to specification generation
- **THEN** the system rejects the plan before starting any agent

#### Scenario: Merge parallel writers
- **WHEN** two code-generation agents each commit their part in their own worktree
- **THEN** both branches are merged into the task branch in agent-name order and their worktrees and branches are removed

#### Scenario: Conflicting writers
- **WHEN** two code-generation agents change the same lines
- **THEN** the task branch is left as it was before the merge, the conflict is recorded, and the task returns to code generation

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
Documentation SHALL state that workers run with the user's operating-system permissions, that Cronos tools cannot change status or approvals but a worker's shell could, what the optional write sandbox protects and what it does not, and SHALL recommend a dedicated user or container for untrusted work.

#### Scenario: Read the trust model
- **WHEN** a user reads the integration documentation
- **THEN** it describes what the gates protect against, what the write sandbox adds, and what requires operating-system isolation

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

### Requirement: Complete and publish
After final approval and documentation, the system SHALL finish the task by the project's publish mode and mark it Completed: `local` merges locally, `push` merges locally and pushes the base branch to the remote, and `pull-request` pushes the task branch and opens a pull request instead of merging. A dirty base SHALL leave the task waiting at local merge; a merge conflict SHALL be aborted and return the task to code generation.

#### Scenario: Complete an approved task locally
- **WHEN** the project publishes locally and final approval, documentation, and local merge succeed
- **THEN** the task is marked Completed and nothing is pushed

#### Scenario: Merge and push
- **WHEN** the project publishes by push and the local merge succeeds
- **THEN** the base branch is pushed to the project's remote and the task is marked Completed with the push recorded

#### Scenario: Open a pull request
- **WHEN** the project publishes by pull request
- **THEN** the task branch is pushed, a pull request into the base branch is opened, the base branch is unchanged locally, and the task is marked Completed with the pull request URL

#### Scenario: Base repository is dirty
- **WHEN** the base repository has uncommitted changes at local merge
- **THEN** the task stays In Progress at local merge and the user can retry after cleaning the base

#### Scenario: Merge conflicts
- **WHEN** the local merge conflicts
- **THEN** the merge is aborted, the base is unchanged, and the task returns to code generation to reconcile with the base branch

#### Scenario: Merge fails without a conflict
- **WHEN** the local merge fails and leaves no unmerged paths
- **THEN** the merge is aborted, the base is unchanged, the failure is recorded, and the task is marked Error

### Requirement: Recover from publishing failures
A failed push or pull request SHALL be recorded in agent activity and mark the task Error at local merge. Retrying the step SHALL NOT merge again or open a second pull request. Git and the GitHub CLI SHALL run without interactive credential prompts.

#### Scenario: Publishing fails
- **WHEN** the push or pull request fails
- **THEN** the failure is recorded in agent activity and the task is marked Error at local merge

#### Scenario: Retry publishing
- **WHEN** a user retries the step after a failed publish
- **THEN** a merge that already happened is not repeated, an open pull request is reused, and publishing completes

#### Scenario: Missing credentials
- **WHEN** pushing needs credentials that are not configured
- **THEN** the push fails without prompting and the task is marked Error

### Requirement: Release merged tasks
When a project has releases on, after a task merges locally the system SHALL tag the result as the next semantic version computed from the Conventional Commits since the last `vX.Y.Z` tag, SHALL prepend that version's changes to `CHANGELOG.md` in a `chore(release)` commit, and SHALL record the version on the task. Push publishing SHALL also push the tag. Retrying the step SHALL reuse a release already tagged on HEAD.

#### Scenario: Release a feature
- **WHEN** a task whose commits include a `feat` commit merges in a project with releases on and latest release `v1.4.2`
- **THEN** `CHANGELOG.md` gains a `v1.5.0` section listing the task's commits, the release commit is tagged `v1.5.0`, and the task records `v1.5.0`

#### Scenario: Breaking change
- **WHEN** a merged commit is marked breaking with `!` or a `BREAKING CHANGE:` footer
- **THEN** the next release bumps the major version

#### Scenario: First release
- **WHEN** the repository has no release tags
- **THEN** the release starts from 0.0.0 and lists only the task's merged commits

#### Scenario: Retry after a failed push
- **WHEN** pushing a released merge fails and the user retries the step
- **THEN** the existing release is reused, no second release commit or tag is created, and the tag is pushed with the base branch

#### Scenario: Releases off
- **WHEN** a project has releases off
- **THEN** merging creates no tag and does not touch `CHANGELOG.md`

### Requirement: Ordered parallel task execution
The system SHALL run at most the project's parallel capacity of tasks at the same time, counting In Progress and Error tasks, and SHALL advance each task through its workflow steps in order, starting a step only after the preceding step of that task is complete. A ready task SHALL NOT start while an active task shares one of its requirements. Each running task SHALL have its own orchestrator.

#### Scenario: Fill the project's tracks
- **WHEN** a project allows three tasks at a time and four independent tasks are ready
- **THEN** three tasks start and the fourth waits until one of them completes or is abandoned

#### Scenario: Default to one task
- **WHEN** a project keeps the default capacity of one and several tasks are ready
- **THEN** the system processes one task at a time and does not start the next until the current one is completed or abandoned

#### Scenario: Avoid conflicting work
- **WHEN** a ready task shares a requirement with an active task
- **THEN** it waits and another ready task without that requirement may start instead

#### Scenario: Error holds a track
- **WHEN** an active task fails and is marked Error
- **THEN** it keeps its track until a human recovers or abandons it

#### Scenario: Advance workflow steps
- **WHEN** a task step completes successfully
- **THEN** the next defined step of that task starts

### Requirement: Worker write sandbox
When a project's sandbox is on, the system SHALL run every worker inside an operating-system write sandbox that denies writes to Cronos-AI's data directory except the worker's own worktree, to the project's base checkout except its Git directory, and to the Git directory's hooks and configuration. The system SHALL refuse to run a step for a sandboxed project when the platform's sandbox tool is unavailable.

#### Scenario: Protected writes are refused
- **WHEN** a sandboxed worker tries to write Cronos-AI's data, the base checkout, or a Git hook
- **THEN** the write fails

#### Scenario: Work and commits still succeed
- **WHEN** a sandboxed worker writes files in its worktree, build caches, or temporary files and commits
- **THEN** the writes and the commit succeed

#### Scenario: Sandbox tool missing
- **WHEN** a sandboxed project's step starts on a machine without the sandbox tool
- **THEN** the step does not start and the user is told what to install
