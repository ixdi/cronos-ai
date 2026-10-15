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
The step planner SHALL be shown the profiles permitted for the current workflow step and SHALL identify a selected profile by its stable identifier in an agent plan. Rust SHALL reject a plan with a profile that is missing, unknown, or not permitted for that step before starting any agent; the step then runs its default plan.

#### Scenario: Select a profile for a step
- **WHEN** the step planner is asked to plan a step
- **THEN** its prompt lists only the profiles permitted for that step

#### Scenario: Reject a profile assigned to another step
- **WHEN** an agent plan selects a profile that is not permitted for its step
- **THEN** Cronos-AI rejects the plan before starting any agent in that step and runs the default plan instead

### Requirement: Load only profile-assigned Pi resources
A worker SHALL receive only its step's built-in skill and the skills, extensions, and MCP configuration assigned by its selected profile, in addition to Cronos-AI's required workflow behavior. Cronos-AI SHALL pass resource values as separate arguments, SHALL NOT install user-configured resources automatically, and SHALL fail before launch when a configured resource is missing or invalid.

#### Scenario: Launch with assigned resources
- **WHEN** a valid agent plan selects a profile with skills or extensions
- **THEN** the worker loads its step's built-in skill and those configured resources, and no unassigned discovered skills or extensions

#### Scenario: Launch without a profile
- **WHEN** an agent plan omits its optional profile identifier
- **THEN** the worker starts with its step's built-in skill and without optional user-configured skills, extensions, or MCP servers

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

### Requirement: Manage agent profiles in the TUI
The TUI SHALL let a user list, create, edit, and delete agent profiles, including their description, allowed workflow steps, skill paths, extension paths, MCP adapter and configuration, model, and time limit. Cronos-AI SHALL validate a profile completely before saving it, SHALL save the catalog atomically, and SHALL let the user open and fix a catalog that contains an invalid profile.

#### Scenario: Create a profile
- **WHEN** a user creates a valid profile in the TUI
- **THEN** it is saved to the profile catalog and is available to the next workflow step

#### Scenario: Edit a profile's resources
- **WHEN** a user adds or removes skill or extension paths, or sets an MCP configuration, and saves
- **THEN** the catalog contains exactly the edited resources

#### Scenario: Invalid profile is not saved
- **WHEN** a user saves a profile with a missing resource path, an invalid ID or model, no workflow step, or only one of the MCP adapter and configuration
- **THEN** the catalog is unchanged and the editor stays open with the error

#### Scenario: Fix a broken catalog
- **WHEN** the catalog contains a profile whose resource no longer exists
- **THEN** the TUI lists every profile, marks the invalid one with its error, and lets the user edit or delete it

#### Scenario: Delete a profile
- **WHEN** a user confirms deleting a profile
- **THEN** it is removed from the catalog and agents can no longer select it

### Requirement: Built-in step skills
Cronos-AI SHALL ship one built-in Pi skill for each agent-running workflow step and SHALL load the step's skill into every worker of that step, before any profile-assigned skills. Cronos-AI SHALL write the built-in skills from its binary into its data directory, SHALL restore a copy whose content differs, and SHALL refuse to write through a symlink.

#### Scenario: Worker loads its step skill
- **WHEN** a workflow step launches a worker, with or without a profile
- **THEN** the worker loads the built-in skill for that step

#### Scenario: Edited skill is restored
- **WHEN** a built-in skill file in the data directory was changed and a workflow step starts
- **THEN** the file is rewritten with the content shipped in the binary

#### Scenario: Symlinked skill path
- **WHEN** a built-in skill directory in the data directory is a symlink
- **THEN** Cronos-AI writes nothing through it and the step fails before any agent starts
