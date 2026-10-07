# Spec Delta

## Purpose

Accept software work through an HTTP interface and preserve it as queued work that Cronos can retrieve and process reliably.

## ADDED Requirements

### Requirement: Submit work through HTTP intake
The system SHALL accept valid work submissions through an HTTP intake interface, persist each accepted submission as a queued task, and return an identifier for that task.

#### Scenario: Accepted submission is queued
- **WHEN** a caller submits a valid work request
- **THEN** the system persists one queued task and returns its identifier

### Requirement: Reject invalid work submissions
The system SHALL reject a submission that is missing required task content and SHALL NOT create a queue item for it.

#### Scenario: Required task content is missing
- **WHEN** a caller submits a request without required task content
- **THEN** the system returns a client error and no task is added to the queue

### Requirement: Preserve queued work across restart
The system SHALL make accepted queued tasks available for processing after the service restarts.

#### Scenario: Pending task survives service restart
- **WHEN** a task has been accepted but not yet started and the service restarts
- **THEN** the task remains queued and available to the workflow

### Requirement: Restrict intake to the local host
The system SHALL expose the HTTP intake interface only on the local host and SHALL NOT accept submissions through a network-facing interface in the first slice.

#### Scenario: Remote host cannot submit work
- **WHEN** a client attempts to reach the intake service from another host
- **THEN** the service is not reachable through a network interface
