# Spec Delta

## Purpose

Defines Cronos AI as the single supported installable, importable, executable, and displayed identity of the application, replacing the prior AI Software Factory public names without aliases.

## ADDED Requirements

### Requirement: Use canonical distribution, import, and command names
The application SHALL be distributed as `cronos-ai`, importable as `cronos_ai`, and invokable through the `cronos-ai` command. Its command help and version output SHALL identify the canonical command name.

#### Scenario: Install and invoke Cronos AI
- **WHEN** a user installs the project package and runs `cronos-ai --help` or `cronos-ai --version`
- **THEN** the command succeeds and reports the Cronos AI command identity

### Requirement: Do not expose legacy public aliases
The application SHALL NOT provide `factory` or `ai-software-factory` executable aliases, the `ai_software_factory` import path, or legacy public entry points as compatibility shims.

#### Scenario: Use a removed command or import name
- **WHEN** a user tries to invoke an old executable alias or import the old package name
- **THEN** the application SHALL NOT resolve that name to the Cronos AI implementation

### Requirement: Use Cronos AI for active product identity
Active user-facing documentation and component-facing identity labels SHALL use Cronos AI naming for the application, including the Herdr workspace label, MCP client identity, sandbox image identity, and generated Git author identity.

#### Scenario: Connect Cronos AI to an external component
- **WHEN** the application creates a Herdr workspace, identifies its MCP client, builds its sandbox image, or generates Git commits
- **THEN** the reported product identity SHALL use Cronos AI naming
