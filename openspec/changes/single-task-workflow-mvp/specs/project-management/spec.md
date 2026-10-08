# Spec Delta

## Purpose

Lets a user create and manage local Cronos-AI projects and tasks, persist their state, and monitor workflow progress through the CLI/TUI.

## ADDED Requirements

### Requirement: Project management
The system SHALL let a user create, view, update, select, and archive projects, and SHALL preserve project data between launches.

#### Scenario: Reopen a project
- **WHEN** a user selects a previously created project after restarting Cronos-AI
- **THEN** the project and its saved tasks are available with their prior state

#### Scenario: Archive a project
- **WHEN** a user archives a project
- **THEN** the project is no longer offered as active work and its stored history remains available

### Requirement: Task management
The system SHALL let a user add tasks to a project and view their priority, dependencies, and current status.

#### Scenario: Create a task
- **WHEN** a user adds a task to a project
- **THEN** the task is saved with a unique identity and an initial Pending status

#### Scenario: Respect task dependencies
- **WHEN** a task depends on another task that is not complete
- **THEN** the dependent task is not selected for execution

### Requirement: Workflow monitoring
The system SHALL show the selected project's tasks, statuses, and current workflow activity, and SHALL refresh that information as the workflow changes.

#### Scenario: View task progress
- **WHEN** a user views a project while a task is running
- **THEN** the task status and active workflow step are visible

### Requirement: Required-tool prompt
At startup, the system SHALL detect missing required tools and prompt the user to install them before attempting to use the affected workflow.

#### Scenario: Required tool is missing
- **WHEN** a required tool is unavailable
- **THEN** the user is told which tool is missing and prompted to install it before the affected workflow proceeds
