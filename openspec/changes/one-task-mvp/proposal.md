# Proposal

## Why

Cronos AI has a product vision but no implementation or executable end-to-end workflow yet. A single-task MVP will validate the central promise—turning submitted work into a human-reviewed GitHub pull request—before adding parallel task execution or a full TUI.

## What Changes

- Add local-only HTTP task submission and SQLite-backed queue persistence, with queue helpers to save and retrieve items.
- Add a LangGraph.js workflow that coordinates one task's agent roles and lifecycle, including human approval or rejection through a small CLI.
- Run each task in an isolated workspace, creating Herdr panes on demand and closing them after use.
- Put workspace management behind a provider boundary so Herdr can be replaced without changing workflow logic; keep agent execution behind a separate boundary.
- Support one configured repository and base branch, and create a GitHub pull request after approval and completion. Stop workflow tracking after PR creation.

Out of scope for this slice: parallel task execution, the full TUI, and post-PR/CI monitoring. Pi is the initial CLI agent runner for coding roles and uses a separately configured chat model; Jev is used for triage classification.

## Capabilities

### New Capabilities
- `task-intake`: Submit tasks over HTTP and persist/retrieve queued items using SQLite.
- `supervised-task-delivery`: Execute one task through LangGraph-coordinated roles in an isolated, replaceable workspace; support human review and create a GitHub PR.

### Modified Capabilities

None. The repository currently has no OpenSpec capability specs.

## Impact

This establishes the first implementation scope for the planned Bun workspace packages: `cronos-core`, `cronos-queue`, `cronos-runtime`, `cronos-storage`, and a minimal CLI in `cronos-panel` (the full TUI is deferred). It introduces SQLite-backed task/workflow persistence and integration boundaries for Herdr workspaces and GitHub PR creation, authenticated with a repository-scoped GitHub App. It selects Pi as the initial CLI runner without prescribing its launch protocol or dependency versions.
