# Spec Delta

## Purpose

Preserves locally queued requests, human actions, and controller state when Cronos AI adopts its canonical default state directory, while preventing ambiguous or destructive directory handling.

## ADDED Requirements

### Requirement: Use the Cronos AI default state directory and override
When no explicit state directory is provided, the application SHALL use `$XDG_STATE_HOME/cronos-ai` when `XDG_STATE_HOME` is set, or `~/.local/state/cronos-ai` otherwise. The application SHALL honor `CRONOS_AI_STATE_DIR` as the named environment override and SHALL NOT honor the legacy `AI_SOFTWARE_FACTORY_STATE_DIR` variable.

#### Scenario: Resolve the default state location
- **WHEN** the user starts a command without an explicit state override
- **THEN** the application uses the Cronos AI state directory under the configured XDG state home or the user's local state directory

#### Scenario: Configure a custom state directory
- **WHEN** the user sets `CRONOS_AI_STATE_DIR` or provides `--state-dir`
- **THEN** the application uses that explicit directory instead of migrating or selecting a default directory

#### Scenario: Ignore the legacy state override
- **WHEN** only `AI_SOFTWARE_FACTORY_STATE_DIR` is set
- **THEN** the application does not treat that variable as an override

### Requirement: Migrate the prior default state directory without data loss
When resolving the default state directory, the application SHALL move an existing `ai-software-factory` default state directory to the corresponding `cronos-ai` location if the new location does not exist. It SHALL preserve the directory contents and existing database format, and SHALL NOT merge or overwrite state directories.

#### Scenario: Migrate existing default state
- **WHEN** the prior default directory exists and the corresponding Cronos AI directory does not
- **THEN** the application moves the existing directory to the Cronos AI location and continues using its preserved state

#### Scenario: Use an already migrated state directory
- **WHEN** the Cronos AI default directory exists and the prior default directory does not
- **THEN** the application uses the Cronos AI directory without migrating data

#### Scenario: Both default directories exist
- **WHEN** both the prior and Cronos AI default directories exist
- **THEN** the application stops with an actionable conflict error and leaves both directories unchanged

#### Scenario: No prior state exists
- **WHEN** neither default directory exists
- **THEN** the application selects the Cronos AI location and creates it as needed without creating a legacy directory
