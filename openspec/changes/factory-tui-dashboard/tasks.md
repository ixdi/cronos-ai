# Tasks

## 1. Isolated implementation setup

- [x] 1.1 Create a dedicated Git branch and worktree for implementation, make the OpenSpec change artifacts available there without carrying unrelated working-tree changes, and verify the branch/path with `git worktree list` and `git status --short`.
- [x] 1.2 Inspect repository/CI for commitlint configuration, add no unrelated commit tooling, and record the commit subject convention to use; verify each eventual implementation commit with configured commitlint or, if absent, the Conventional Commit `type(scope): description` format.
- [x] 1.3 Add Textual in the package dependency metadata and lockfile, then verify `uv sync --locked` and the dependency audit pass in the implementation worktree.

## 2. Durable and safe activity history

- [x] 2.1 Add failing storage tests for the activity-event migration, UTC identity/timestamp fields, run/task filtering, and bounded pagination; verify the new tests fail for the missing behavior before implementation.
- [x] 2.2 Implement the additive SQLite migration and indexed event append/query APIs; verify the migration tests pass against both a new database and a database at the previous schema version.
- [x] 2.3 Add failing sanitization tests for provider-token formats, environment secrets, oversized summaries, and raw tool payload exclusion; verify they fail before implementing the sanitizer.
- [x] 2.4 Implement fail-closed summary sanitization and record concise task lifecycle and worker progress events at durable event boundaries; verify the sanitization tests pass and raw RPC/tool payloads are not persisted.
- [x] 2.5 Commit the completed activity-history slice with a commitlint-valid Conventional Commit subject, and verify the commit contains only intended source/tests and no state database, logs, secrets, or temporary files.

## 3. Dashboard read model and progress derivation

- [x] 3.1 Add failing read-model tests for run aggregation, current stage selection, worker/task counts, completed-task percentage rounding, unknown stage, and human-attention state; verify the tests fail before adding the read model.
- [x] 3.2 Implement bounded read snapshots from existing run, task, attempt, review, context, controller, attention, and activity data; verify read-model tests cover concurrent runs and taskless/unavailable details.
- [x] 3.3 Add tests proving snapshot refreshes and dashboard queries do not mutate requests, tasks, attempts, approvals, or control actions; verify persisted state is unchanged before and after repeated reads.
- [x] 3.4 Commit the read-model slice with a commitlint-valid Conventional Commit subject and verify the commit is isolated to the intended worktree and change.

## 4. Interactive dashboard

- [x] 4.1 Add UI tests for the overview, empty state, controller health, run/task selection, activity pagination, review/output availability, and safe display of sanitized summaries; verify keyboard navigation and narrow-terminal behavior.
- [x] 4.2 Implement the Textual dashboard with refreshable overview and run/task details, bounded background reads, explicit status/empty/error states, and read-only navigation; verify the UI tests pass and refresh does not block input.
- [ ] 4.3 Document dashboard invocation, refresh/navigation keys, completion-percentage meaning, read-only behavior, log-safety limits, state-directory override, and terminal requirements; verify each documented command/option matches CLI help.
- [ ] 4.4 Commit the dashboard UI and its tests/documentation with a commitlint-valid Conventional Commit subject and verify the commit contains no generated runtime output.

## 5. CLI integration and end-to-end validation

- [ ] 5.1 Add failing CLI tests for the dashboard command, state-directory override, interactive-terminal rejection, and preservation of existing command help/output; verify the tests fail before CLI integration.
- [ ] 5.2 Expose the dashboard through the canonical `cronos-ai` CLI and wire graceful shutdown and error reporting; verify CLI tests pass and existing non-interactive CLI tests remain unchanged and green.
- [ ] 5.3 Add an end-to-end test with concurrent persisted runs, progress events, blocked work, human attention, and review/output references; verify displayed counts, stage, percentage, event history, and read-only behavior match the database state.
- [ ] 5.4 Commit the CLI integration slice with a commitlint-valid Conventional Commit subject and verify all implementation commits are present on the dedicated branch.
- [ ] 5.5 Run `uv run pytest`, `uv run ruff check .`, `uv run mypy`, `uv build --no-sources`, and `uv audit --locked`; verify all pass, then review `git status --short` and `git log` for a clean implementation worktree and compliant commits.
