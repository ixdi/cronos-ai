# Proposal

## Why

The checkout directory is already named `cronos-ai`, but the installable distribution, Python package, CLI, state location, and user-facing materials still identify as AI Software Factory.
A consistent Cronos AI identity should make installation and everyday use match the project name while preserving users' queued work and controller state through the rename.

## What Changes

- **BREAKING:** Rename the Python distribution to `cronos-ai`, import package to `cronos_ai`, and primary CLI executable to `cronos-ai`; do not provide `factory` or `ai-software-factory` command aliases, old import compatibility, or old `AI_SOFTWARE_FACTORY_*` environment-variable aliases.
- Update package metadata, version lookup, MCP client identity, Herdr labels, sandbox image identity, generated Git author identity, tests, and active documentation to use Cronos AI naming.
- Rename the active workflow diagram source and rendered asset to Cronos AI filenames and update their documentation links.
- Use `CRONOS_AI_STATE_DIR` and the default `cronos-ai` state directory. On first use of the default location, move the prior default `ai-software-factory` state directory to the new location without changing its contents or SQLite schema. Fail safely with an actionable error if both default directories exist; do not merge or overwrite either directory.
- Keep the existing database filename and webhook request-ID namespace stable because they are persisted data identifiers, not public command or import aliases.
- Preserve existing CLI functionality, controller behavior, security controls, and request/task state behavior under the new names.
- Do not rename the checkout directory, which is already `cronos-ai`, or rewrite archived OpenSpec history.

## Capabilities

### New Capabilities

- `application-identity`: Defines the canonical install, import, command, and displayed product identity and removes the old public names.
- `local-state-migration`: Defines safe migration of the default local state directory while preserving durable factory data.

### Modified Capabilities

None. The repository has no main specs yet; these requirements establish new capabilities for the renamed application's public identity and state migration.

## Impact

The change affects `pyproject.toml`, `uv.lock`, `src/ai_software_factory/` and its tests, `README.md`, `AGENTS.md`, `factory.md`, `design/factory.md`, `docs/factory-guide.html`, the workflow diagram source and image, and runtime identity strings used by the MCP bridge, Herdr adapter, sandbox, and Git integration.
It changes the installed command, import path, environment-variable name, and default state path, while retaining persisted database contents and webhook idempotency semantics.
