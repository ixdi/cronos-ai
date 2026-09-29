# Design

## Context

See `proposal.md` for motivation and change boundaries.
The checkout root is already `cronos-ai`; the installed distribution and version lookup are `ai-software-factory`, the import package is `ai_software_factory`, the CLI parser advertises `factory`, and the package exposes both `factory` and `ai-software-factory` scripts.
`default_state_dir()` currently reads `AI_SOFTWARE_FACTORY_STATE_DIR` and selects an `ai-software-factory` directory below the XDG state root or `~/.local/state`.
CLI subcommands store durable records in `factory.sqlite3`.
The package name and internal imports are referenced throughout `src/`, `tests/`, `README.md`, and the HTML guide.
There are no main specs; this change adds identity and state-migration capabilities.

## Goals / Non-Goals

**Goals:**

- Present one canonical install, import, executable, environment-variable, and active product identity.
- Keep all existing CLI operations and durable records available under the new names.
- Migrate the default state directory without merging, overwriting, or changing the SQLite schema.
- Keep webhook event retries idempotent across the rename.

**Non-Goals:**

- Retain old command, import, or environment-variable aliases.
- Rename the already-canonical checkout directory or rewrite archived OpenSpec history.
- Rename generic domain concepts, class names, the database filename, database schema, or persisted webhook request-ID namespace.
- Introduce new runtime dependencies or change workflow behavior unrelated to identity and state location.

## Decisions

- Use `cronos-ai` as the distribution and executable name, `cronos_ai` as the import package, and `Cronos AI` for human-readable product labels. Update argparse's program name as well as package entry points so help, errors, version output, and command examples agree. Remove both existing console entry points rather than preserving `factory` as an alias, as requested.
- Rename the `src/ai_software_factory` package directory and update internal imports, tests, module execution, metadata version lookup, and the generated lockfile together. A package metadata-only rename would leave `python -m` execution and imports inconsistent; compatibility shims are explicitly excluded.
- Rename the runtime override to `CRONOS_AI_STATE_DIR`. Resolve explicit `--state-dir` or the new environment override before default migration so custom locations bypass default-path migration. Do not consult `AI_SOFTWARE_FACTORY_STATE_DIR`; users with custom state locations can select the existing directory using the new override.
- For default state, derive old and new sibling paths from the same XDG state root (or the same `~/.local/state` root). If only the old path exists, move the whole directory to the new path, preserving `factory.sqlite3` and its schema. If only the new path exists, use it. If neither exists, select the new path and create it when needed. If both exist, fail with an actionable conflict message and leave both untouched rather than guessing which state is authoritative.
- Serialize migration attempts and make the directory move no-clobber, so concurrent CLI starts or a pre-existing empty destination cannot overwrite or merge state. Do not change the effective path for an explicit state override.
- Keep the webhook UUID5 namespace string stable even though it contains the prior product name. It contributes to deterministic persisted request IDs; changing it would make a retried webhook generate a different request ID after upgrade and could defeat deduplication. This is an internal persistence invariant, not a command or compatibility alias.
- Update owned active documentation and runtime identity surfaces, including README install/build examples, the user guide, agent instructions, the workflow source and rendered image filenames, Herdr labels, MCP client identity, sandbox image tags, and generated Git author identity. Leave archived change artifacts as historical records.
- Validate with unit and integration tests for canonical and absent legacy entry points, state path selection and conflict/migration behavior, durable queue recovery, and webhook idempotency. Also build and install a wheel in a clean environment and run the full existing suite to catch package-path regressions.

Alternatives considered: keep the old executable or import as a forwarding alias, rejected because the user explicitly requested no aliases; copy state instead of moving it, rejected because two mutable state trees can diverge; rename the database and webhook ID namespace, rejected because neither is needed for the directory migration and both risk breaking persisted records.

## Risks / Trade-offs

- [Existing scripts, Python imports, or integrations using old names will stop working] → Treat this as an intentional breaking rename, update all active project documentation and tests, and verify the installed wheel exposes only the new public names.
- [A state directory may be partially or concurrently migrated] → Serialize the migration, use a no-clobber move, preserve the source on error, and test interrupted/conflicting cases before using the resulting store.
- [Users with a custom legacy environment override may not find their state automatically] → Do not silently honor the old variable; document setting `CRONOS_AI_STATE_DIR` to the existing custom path.
- [Changing a persisted webhook namespace could admit duplicate logical requests] → Keep the namespace constant and test replay after restart.

## Migration Plan

1. Rename package metadata, source package, CLI entry point, and active references; regenerate `uv.lock`.
2. Add and test default-path migration and conflict handling while preserving the existing database file and schema.
3. Update active documentation and verify clean-environment wheel install, command help/version, legacy-name absence, and full tests.
4. For rollback before Cronos AI has written new-path state, move the new state directory back to the old path after confirming the old path is absent, then reinstall the prior package. If new state has been written, stop the application and make a backup before reconciling directories; never overwrite either tree automatically.
