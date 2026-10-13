# application-launcher Specification

## Purpose

Provides a reliable Cronos-AI startup path that runs supervised TUI sessions in Herdr and avoids nesting Herdr when the launcher is already inside it.

## Requirements

### Requirement: Start directly inside Herdr
The startup script SHALL detect a Herdr-managed caller and launch the Cronos-AI TUI in the current pane without creating or attaching to another Herdr session or workspace.

#### Scenario: Launch from a Herdr pane
- **WHEN** a user runs the startup script from a Herdr-managed pane
- **THEN** Cronos-AI starts in that pane and no additional Herdr session or workspace is created

### Requirement: Launch Cronos-AI in Herdr from outside
When the startup script is run outside a Herdr-managed pane, it SHALL start or attach to the dedicated Cronos-AI Herdr session, create a dedicated workspace and pane, launch the TUI there, and open the Herdr interface for that session without taking over unrelated user panes.

#### Scenario: Launch outside Herdr
- **WHEN** a user runs the startup script from a regular terminal
- **THEN** Herdr opens or attaches to the Cronos-AI session and displays the TUI running in its dedicated workspace

#### Scenario: Reuse the named Herdr session
- **WHEN** the Cronos-AI Herdr session already exists
- **THEN** the launcher attaches to that named session and starts Cronos-AI in a dedicated workspace without replacing existing panes

### Requirement: Report startup failures safely
If the Cronos-AI binary or Herdr cannot be run, the startup script SHALL report the failure and exit unsuccessfully without installing tools automatically or silently starting the TUI outside Herdr.

#### Scenario: Herdr is unavailable
- **WHEN** the launcher cannot find or start Herdr
- **THEN** it identifies the missing or failed tool, exits unsuccessfully, and leaves installation to the user
