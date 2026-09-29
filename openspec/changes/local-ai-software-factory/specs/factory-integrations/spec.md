# Spec Delta

## Purpose

Connects the local factory to production alerts and each target repository's existing delivery pipeline. It turns trusted, validated integration events into ordinary work while leaving deployment behavior under the repository's established CI/CD system.

## ADDED Requirements

### Requirement: Receive provider-neutral monitoring webhooks
The factory SHALL accept monitoring events through a provider-neutral webhook contract at a user-configured reachable endpoint. The endpoint SHALL authenticate each source, reject invalid or replayed events, and deduplicate repeated deliveries before creating work.

#### Scenario: Accept a valid alert
- **WHEN** an authenticated monitoring source sends a valid, previously unseen alert
- **THEN** the factory records one normalized event and submits it to normal triage

#### Scenario: Reject an invalid or replayed alert
- **WHEN** a webhook has invalid authentication, an invalid timestamp, or a previously consumed event identifier
- **THEN** the factory rejects or deduplicates it without creating another task

#### Scenario: Receive an alert while offline
- **WHEN** the local controller is not reachable
- **THEN** the factory does not claim to have received the alert and relies on source retry or human resubmission after service is restored

### Requirement: Treat external event content as untrusted
The factory SHALL validate webhook payloads against an allowlisted schema and SHALL treat alert descriptions and attached external content as untrusted data. An alert SHALL enter the normal planning, approval, and execution gates and SHALL NOT directly execute commands or modify production.

#### Scenario: Alert content contains instructions
- **WHEN** an alert contains text that attempts to direct the agent or override factory policy
- **THEN** the content is handled as incident data and cannot bypass the normal approval and security controls

#### Scenario: Alert describes high-impact work
- **WHEN** triage determines that an alert response requires a high-impact change
- **THEN** the task waits for the required human approval before implementation

### Requirement: Use the target repository's existing CI/CD
The factory SHALL invoke only a target repository's explicitly configured CI/CD workflow after human review. It SHALL wait for an authoritative result and SHALL report failure or missing configuration as blocked rather than claiming successful delivery.

#### Scenario: CI/CD succeeds
- **WHEN** an approved run is submitted to the configured CI/CD workflow and that workflow succeeds
- **THEN** the factory records successful delivery and may mark the run done

#### Scenario: CI/CD fails or is not configured
- **WHEN** the configured workflow fails or no workflow is configured
- **THEN** the factory does not mark the run shipped and exposes the reason for human action

### Requirement: Do not replace deployment ownership
The factory SHALL leave build, release, and deployment semantics to the target repository's configured CI/CD workflow. It SHALL NOT claim delivery success before receiving that workflow's successful result.

#### Scenario: Existing workflow owns deployment
- **WHEN** a target repository's configured CI/CD workflow performs deployment
- **THEN** the factory reports the workflow result without implementing a separate deployment path
