# agent-resources Specification

## Purpose

Lets users assign reviewed Pi skills, extensions, and MCP server configurations to named workflow-agent profiles without giving every worker the same tools or relying on implicit startup scripts.

## Requirements

### Requirement: User-managed agent profiles
Cronos-AI SHALL load user-editable agent profiles from its data directory. Each profile SHALL have a stable identifier, the workflow steps where it may be used, and references to its Pi skills, extensions, and optional MCP configuration. A profile MAY set a Pi model and an agent time limit in minutes.

#### Scenario: Add or edit a profile
- **WHEN** a user adds or updates a valid profile in the profile catalog
- **THEN** Cronos-AI makes that profile available to subsequent workflow steps without a source-code change

#### Scenario: Profile changes take effect
- **WHEN** a user changes a profile between agent launches
- **THEN** subsequent agent launches use the updated profile while already-running agents remain unchanged

#### Scenario: Profile selects a model and time limit
- **WHEN** a selected profile sets a model and `timeout_minutes`
- **THEN** the worker starts with that model and is canceled if it exceeds that limit

### Requirement: Identify profiles for workflow agents
The orchestrator SHALL be able to discover profiles permitted for the current workflow step and SHALL identify a selected profile by its stable identifier in an agent plan. Rust SHALL reject a profile that is missing, unknown, or not permitted for that step before starting that agent.

#### Scenario: Select a profile for a step
- **WHEN** the orchestrator requests available profiles for the current step
- **THEN** Cronos-AI returns only profiles permitted for that step

#### Scenario: Reject a profile assigned to another step
- **WHEN** an agent plan selects a profile that is not permitted for its step
- **THEN** Cronos-AI reports a validation error and does not start any agent in that step

### Requirement: Load only profile-assigned Pi resources
A worker SHALL receive only the skills, extensions, and MCP configuration assigned by its selected profile, in addition to Cronos-AI's required workflow behavior. Cronos-AI SHALL pass resource values as separate arguments, SHALL NOT install resources automatically, and SHALL fail before launch when a configured resource is missing or invalid.

#### Scenario: Launch with assigned resources
- **WHEN** a valid agent plan selects a profile with skills or extensions
- **THEN** the worker loads those configured resources and no unassigned discovered skills or extensions

#### Scenario: Launch without a profile
- **WHEN** an agent plan omits its optional profile identifier
- **THEN** the worker starts without optional user-configured skills, extensions, or MCP servers

#### Scenario: Configured resource is unavailable
- **WHEN** a selected profile refers to a missing or invalid resource
- **THEN** Cronos-AI reports the profile and resource error and starts no agent in that step

### Requirement: Isolate per-profile MCP servers
An agent profile SHALL be able to select a Pi-compatible MCP configuration containing the servers for that profile. A worker SHALL NOT connect to MCP servers from unrelated global, project, or profile configurations, and a configured MCP adapter or server failure SHALL be reported without silently launching a worker with a different server set.

#### Scenario: Start an agent with selected MCP servers
- **WHEN** a profile with an MCP configuration is selected
- **THEN** only servers from that profile's MCP configuration are available to the worker

#### Scenario: MCP support is unavailable
- **WHEN** a profile requests MCP resources but the configured Pi MCP adapter or configuration cannot be used
- **THEN** Cronos-AI reports the failure and does not start the worker
