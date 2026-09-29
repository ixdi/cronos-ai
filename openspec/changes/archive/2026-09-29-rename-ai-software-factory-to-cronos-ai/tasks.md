# Tasks

## 1. Rename the package and public CLI

- [x] 1.1 Rename `src/ai_software_factory/` to `src/cronos_ai/` and update imports, module execution, package metadata, version lookup, argparse program name, and console scripts; regenerate `uv.lock` and update CLI tests; verify `uv sync --locked`, `uv run cronos-ai --help`, `python -m cronos_ai --version`, and `uv run pytest tests/test_cli.py` succeed.
- [x] 1.2 Update active install, run, import, and development instructions in `README.md` and `docs/factory-guide.html` to use `cronos-ai` and `cronos_ai` only; verify every documented command and example uses the supported names and relative documentation links still resolve.

## 2. Rename runtime identity and workflow assets

- [x] 2.1 Update Cronos AI labels and identifiers for Herdr workspaces, MCP client information, sandbox image/build names, Git author identity, and test environment flags; add or update focused tests for each identity surface and retain the webhook UUID5 namespace unchanged to preserve request idempotency.
- [x] 2.2 Rename the active workflow source and rendered image assets to `cronos_ai_workflow` names, update references in `AGENTS.md`, `factory.md`, `design/factory.md`, and the HTML guide, and update active product headings to Cronos AI; verify every referenced asset exists and no active documentation points to the old filenames.

## 3. Migrate local state safely

- [x] 3.1 Change the state override to `CRONOS_AI_STATE_DIR` and the default directory to `cronos-ai`; ensure explicit `--state-dir` and the new environment override bypass default migration and the old environment override is ignored; verify tests cover XDG, home-directory, explicit-override, and legacy-variable cases.
- [x] 3.2 Implement serialized, no-clobber migration of the prior default state directory, preserving its files and database schema; fail without modifying either directory when both old and new paths exist; verify tests cover old-only migration, new-only reuse, neither-existing initialization, conflict preservation, and concurrent startup safety.
- [x] 3.3 Update active state-location and migration documentation, including custom-path guidance for users who previously used the old environment variable; verify examples use `CRONOS_AI_STATE_DIR` and describe conflict recovery accurately.

## 4. Verify the renamed distribution end to end

- [x] 4.1 Build and install the wheel into a clean virtual environment, verify `cronos-ai --help`, `cronos-ai --version`, and `python -m cronos_ai` work while the old command aliases and import path are absent, then run `uv run pytest`, `uv run ruff check .`, `uv run mypy`, and `uv audit --locked` successfully.
