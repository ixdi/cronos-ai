# Spec Delta

## Purpose

Runs specialized coding agents as observable, bounded workers while isolating their changes from the user's checkout and host environment. It enables concurrent task execution without allowing conflicting changes or unreviewed failures to disappear from human oversight.

## ADDED Requirements

### Requirement: Use curated specialist profiles
Each workflow stage SHALL run through an approved specialist profile that explicitly declares its skills and MCP tools. The factory SHALL NOT silently inherit arbitrary skills, extensions, or MCP servers from the host or target repository.

#### Scenario: Start a specialist with approved resources
- **WHEN** the scheduler launches a task for a specialist profile
- **THEN** the agent receives only the profile's explicitly approved skills and MCP tools

#### Scenario: An unlisted skill or MCP server is discovered
- **WHEN** an unlisted skill, extension, or MCP server is available on the host or target repository
- **THEN** it is not enabled for the worker by default

### Requirement: Control Pi workers with structured progress
The factory SHALL run Pi workers through a machine-readable, bidirectional control interface and SHALL determine task completion from a settled run result rather than a prompt-accepted response or terminal text alone.

#### Scenario: Observe a Pi task to completion
- **WHEN** Pi accepts a task prompt and streams progress events
- **THEN** the factory consumes structured events and marks the agent turn settled only when Pi reports that no automatic work remains

#### Scenario: A Pi process exits unexpectedly
- **WHEN** a Pi worker exits before reporting a settled result
- **THEN** the factory records the worker failure and applies the configured recovery policy

### Requirement: Assign reusable Herdr worker slots
The factory SHALL treat Herdr panes as reusable worker slots with at most one active task per slot. It SHALL report each worker's current task and lifecycle state to Herdr and SHALL reuse a surviving worker association during recovery.

#### Scenario: Dispatch work to an available slot
- **WHEN** a dependency-ready task is scheduled and a Herdr slot is available
- **THEN** the factory assigns the task to that slot and reports its working state and task summary

#### Scenario: A slot is reused
- **WHEN** a worker completes its assigned task and is available for more work
- **THEN** the scheduler may assign another eligible task to the same slot without treating the pane as permanently tied to one agent role

### Requirement: Isolate parallel implementation work
Each concurrently running implementation task SHALL have a separate Git worktree based on the run branch. The factory SHALL keep the user's original branch unchanged during task execution and SHALL integrate clean task results into the run branch in dependency order.

#### Scenario: Run independent implementation tasks concurrently
- **WHEN** multiple implementation tasks are dependency-ready
- **THEN** each active task receives its own worktree and changes do not share a writable checkout

#### Scenario: Integrate a clean task result
- **WHEN** a completed task branch can be merged into the run branch without conflicts
- **THEN** the factory merges it into the run branch before dispatching tasks that depend on its result

#### Scenario: Task integration conflicts
- **WHEN** a completed task branch conflicts with the run branch
- **THEN** the factory preserves both worktrees, marks integration blocked, and requests human resolution rather than silently overwriting changes

### Requirement: Run workers in fail-closed sandboxes
Implementation workers SHALL run in a Docker-compatible isolated environment with access limited to the task worktree and explicitly required resources. The factory SHALL fail closed when the required sandbox runtime or egress policy is unavailable.

#### Scenario: Start an isolated worker
- **WHEN** the factory launches an implementation worker
- **THEN** the worker cannot access the host home directory or unrelated host files through mounted volumes

#### Scenario: Sandbox prerequisites are unavailable
- **WHEN** the container runtime or required network restrictions cannot be established
- **THEN** the factory does not launch the worker outside the sandbox

#### Scenario: Worker network access
- **WHEN** a worker needs network access
- **THEN** outbound destinations are limited to the configured model provider and explicitly approved package or task services

### Requirement: Limit credentials exposed to workers
The factory SHALL NOT mount the host Pi configuration or authentication directory into a worker. It SHALL provide only a separately configured provider API key required for the run, SHALL NOT persist that key in the repository or orchestration database, and SHALL restrict its lifetime to the worker execution where feasible.

#### Scenario: Launch Pi without host authentication files
- **WHEN** the factory starts a Pi worker in a sandbox
- **THEN** the host Pi authentication directory is not mounted and unrelated host credentials are unavailable

#### Scenario: Persist or inspect task state
- **WHEN** task plans, execution records, or logs are persisted
- **THEN** provider API keys are not stored in those records
