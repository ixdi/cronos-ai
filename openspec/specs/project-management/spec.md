# project-management Specification

## Purpose

Lets a user create and manage local Cronos-AI projects and tasks, persist their state, and monitor workflow progress through the CLI/TUI.

## Requirements

### Requirement: Project management
The system SHALL let a user create, view, update, select, and archive projects, and SHALL preserve project data between launches.

#### Scenario: Reopen a project
- **WHEN** a user selects a previously created project after restarting Cronos-AI
- **THEN** the project and its saved tasks are available with their prior state

#### Scenario: Archive a project
- **WHEN** a user archives a project
- **THEN** the project is no longer offered as active work and its stored history remains available

### Requirement: Task management
The system SHALL let a user add tasks to a project and view their priority, dependencies, and current status. Task titles SHALL be trimmed and their whitespace collapsed, and a task whose title matches an open task (one that is not Completed or Rejected) SHALL be rejected.

#### Scenario: Create a task
- **WHEN** a user adds a task to a project
- **THEN** the task is saved with a unique identity, a normalized title, and an initial Pending status

#### Scenario: Respect task dependencies
- **WHEN** a task depends on another task that is not complete
- **THEN** the dependent task is not selected for execution

#### Scenario: Reject a duplicate open task
- **WHEN** a user adds a task whose title matches an open task, ignoring case and spacing
- **THEN** no task is created and the user is told the task already exists

### Requirement: Workflow monitoring
The system SHALL show the selected project's tasks, statuses, and current workflow activity, and SHALL refresh that information as the workflow changes. The task list SHALL offer a table and a kanban board grouped by status, and SHALL summarize the project's progress: completed tasks out of those not abandoned, the average time to complete a task, and the number of tasks in Error.

#### Scenario: View task progress
- **WHEN** a user views a project while a task is running
- **THEN** the task status and active workflow step are visible

#### Scenario: Switch to the board
- **WHEN** a user switches the task list to the board
- **THEN** tasks appear in columns by status, the selection moves between cards and columns with the arrow keys, and opening a card shows that task's details

#### Scenario: See project progress
- **WHEN** a user views a project with completed, failed, and abandoned tasks
- **THEN** the task list shows how many tasks are done out of those not abandoned, the average completion time, and the error count

### Requirement: Required-tool prompt
At startup, the system SHALL detect missing required tools and prompt the user to install them before attempting to use the affected workflow.

#### Scenario: Required tool is missing
- **WHEN** a required tool is unavailable
- **THEN** the user is told which tool is missing and prompted to install it before the affected workflow proceeds

### Requirement: TUI project creation
The TUI SHALL let a user create a project from the project list with a nonblank name and optional description and local Git repository path. It SHALL persist valid submissions and refresh the project list; invalid submissions SHALL show an error and remain available for correction without saving a project.

#### Scenario: Create a project
- **WHEN** a user submits a valid project name, optional description, and optional repository path
- **THEN** the project is saved as active and appears in the project list

#### Scenario: Correct invalid project details
- **WHEN** a user submits a blank name or an invalid repository path
- **THEN** the TUI shows the validation error, preserves the entered details for correction, and does not create a project

#### Scenario: Cancel project creation
- **WHEN** a user cancels project creation before submitting
- **THEN** no project is created and the project list remains unchanged

### Requirement: TUI task creation
The TUI SHALL let a user create a task from an active project's task list with a nonblank title, optional description, priority, and dependencies on tasks in that project. A successful creation SHALL save the task as Pending, show it in the task list, and not start its workflow.

#### Scenario: Create a task
- **WHEN** a user submits a valid task with a title and selected task details
- **THEN** the task is saved as Pending with the chosen priority and dependencies, and appears in the task list without starting its workflow

#### Scenario: Use task creation defaults
- **WHEN** a user creates a task without setting priority or dependencies
- **THEN** the task uses Medium priority and has no dependencies

#### Scenario: Correct invalid task details
- **WHEN** a user submits a blank title or invalid task details
- **THEN** the TUI shows the validation error, preserves the entered details for correction, and does not create a task

#### Scenario: Cancel task creation
- **WHEN** a user cancels task creation before submitting
- **THEN** no task is created and the task list remains unchanged

#### Scenario: Archived project cannot receive a task
- **WHEN** a user requests task creation for an archived project
- **THEN** the TUI explains that the project is archived and does not create a task

### Requirement: Persistent branded TUI title
The TUI SHALL display the title `Cronos-AI` at the top of every dashboard screen using the primary color `#BC93F8`.

#### Scenario: Show the application title
- **WHEN** the user views the projects, tasks, or task-details screen
- **THEN** `Cronos-AI` is visible above the screen content in the primary color

### Requirement: Semantic TUI palette
The TUI SHALL use primary `#BC93F8` for the title and primary emphasis, warning `#FFB86C` for pending or attention-needed states, error `#FE5654` for failed states and error feedback, shortcut `#DE6EB1` for keyboard hints, and success `#006A63` for completed or approved outcomes.

#### Scenario: Render workflow states with semantic colors
- **WHEN** the dashboard displays pending or attention-needed work, an error, or a completed or approved outcome
- **THEN** it uses the corresponding warning, error, or success color from the defined palette

#### Scenario: Distinguish keyboard hints
- **WHEN** the TUI displays keyboard shortcut hints
- **THEN** the shortcut labels use `#DE6EB1` while the action descriptions remain readable

### Requirement: Preserve text cues alongside color
The TUI SHALL retain textual status, error, outcome, and keyboard-action labels so their meaning does not depend on color.

#### Scenario: Read dashboard meaning without color
- **WHEN** the terminal cannot display the palette colors distinctly
- **THEN** status, error, outcome, and keyboard-action text still communicates the same meaning

### Requirement: Project check command
A project SHALL store an optional check command as a program and arguments, run without a shell, and an optional check time limit from 1 to 480 minutes that defaults to 60 minutes when unset. Users SHALL set both from the CLI or the TUI project form. Starting tasks SHALL require a configured check command.

#### Scenario: Configure a check command
- **WHEN** a user sets a project's check command
- **THEN** it is saved as separate arguments and shown with the project

#### Scenario: Start without a check command
- **WHEN** a user starts a task in a project without a check command
- **THEN** the TUI explains that a check command is required and starts nothing

#### Scenario: Configure a check time limit
- **WHEN** a user sets a project's check time limit to a whole number of minutes from 1 to 480
- **THEN** it is saved, shown with the project, and verification stops the check command when that limit is reached

#### Scenario: Reject an invalid check time limit
- **WHEN** a user sets a check time limit of 0, more than 480, or a value that is not a whole number of minutes
- **THEN** the project is unchanged and the user sees why the limit was rejected

#### Scenario: Restore the default limit
- **WHEN** a user clears a project's check time limit
- **THEN** verification stops the check command after 60 minutes

### Requirement: Approval review view
Task details SHALL show the worktree path, branch, OpenSpec change, and the current step's elapsed time. A review view SHALL show the task's OpenSpec change, its commits and diff summary against the base, the latest check-command output, and the full per-file diff against the base, with a way to move between changed files.

#### Scenario: Review before approving
- **WHEN** a task waits at an approval gate and the user opens the review view
- **THEN** the specification, commits, diff summary, latest check output, and each changed file's diff are visible without leaving the TUI

#### Scenario: Move between changed files
- **WHEN** the user jumps to the next or previous file in the review view
- **THEN** the view scrolls to that file's diff and shows which file of how many is on screen

#### Scenario: Very large diff
- **WHEN** the diff is longer than the review view's limit
- **THEN** the view shows the first part and says how many lines were left out

#### Scenario: See step progress
- **WHEN** a user views a running task
- **THEN** the elapsed time of the current step is visible

### Requirement: Serialized store changes
The system SHALL apply every change to stored projects and tasks as one exclusive read-check-save operation, so that changes made at the same time by different processes or threads never overwrite each other. The system SHALL release this exclusivity when its holder exits, including after a crash, and SHALL NOT require it for reads.

#### Scenario: Create tasks at the same time
- **WHEN** two processes add tasks to the same project at the same time
- **THEN** both tasks are saved and the project lists both of them

#### Scenario: Change a task while a workflow step records activity
- **WHEN** a user abandons or approves a task while a workflow step records agent activity for it
- **THEN** the stored task keeps both the user's change and the recorded activity, or the later operation is rejected because the task is no longer in a state that allows it

#### Scenario: Holder crashes
- **WHEN** a process exits while changing the store
- **THEN** the next change made by another process proceeds without manual cleanup

### Requirement: Project publish settings
A project SHALL store a publish mode of `local`, `push`, or `pull-request`, defaulting to `local`, an optional Git remote name that defaults to `origin`, and whether releases are on, defaulting to off. Users SHALL set them from the CLI, and the publish mode and releases from the TUI project form. The system SHALL reject a remote name that is empty, longer than 100 characters, starts with `-` or `.`, or contains characters other than letters, digits, `.`, `_`, and `-`.

#### Scenario: Choose a publish mode
- **WHEN** a user sets a project's publish mode and remote
- **THEN** they are saved and the task overview shows where approved work will be published

#### Scenario: Turn releases on
- **WHEN** a user turns releases on for a project
- **THEN** the setting is saved and later merges are released

#### Scenario: Reject an unsafe remote name
- **WHEN** a user sets a remote name such as `-c` or `my remote`
- **THEN** the project is unchanged and the user sees why the name was rejected

### Requirement: Project requirements
A project SHALL store functional and non-functional requirements that users add, list, and remove from the CLI or the TUI. Requirement text SHALL be normalized, and a requirement that matches an existing one, ignoring case and spacing, SHALL be rejected.

#### Scenario: Add a requirement
- **WHEN** a user adds a non-functional requirement
- **THEN** it is saved with its kind and normalized text and listed with the project

#### Scenario: Reject a duplicate requirement
- **WHEN** a user adds a requirement that differs from an existing one only in case or spacing
- **THEN** the project is unchanged and the user is told it is already listed

### Requirement: Plan tasks from requirements
The system SHALL run a read-only Pi planner in the project repository to propose tasks for the project's requirements, SHALL validate the proposal, and SHALL create tasks only after a human applies it. A valid plan SHALL cover every requirement, reference only known requirements and planned tasks, have no dependency cycle, and propose at most 50 tasks. Applying SHALL create tasks dependencies first, link them to their requirements, and reuse an open task with the same title.

#### Scenario: Plan and apply
- **WHEN** the planner proposes a valid plan and the user applies it
- **THEN** its tasks are created with their priorities, dependencies, and requirement links, and the pending plan is cleared

#### Scenario: Review before creating tasks
- **WHEN** the planner has proposed a plan that nobody has applied
- **THEN** no task exists for it and the user can view or discard it

#### Scenario: Invalid plan
- **WHEN** the planner's answer has no plan, leaves a requirement uncovered, or has a dependency cycle
- **THEN** no plan is kept and the user sees why it was rejected

#### Scenario: Responsive dashboard while planning
- **WHEN** the user starts planning from the TUI
- **THEN** the planner runs in the background, the dashboard stays usable, and the plan appears when it is ready
