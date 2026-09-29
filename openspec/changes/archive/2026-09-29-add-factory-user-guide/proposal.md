# Proposal

## Why

New users lack a clear, user-oriented path from installing the local factory to submitting work, handling approvals, and understanding delivery status.

The existing README, implementation APIs, and root workflow diagram are spread across different files, and the illustrated end-to-end flow can be mistaken for a fully automated CLI workflow.

## What Changes

- Add a repository-rendered Markdown guide at `docs/factory-guide.md` with setup, a first-use path, CLI actions, worker and integration concepts, human gates, security boundaries, and troubleshooting.
- Include a Mermaid workflow diagram that distinguishes implemented CLI behavior from component APIs that still require orchestration.
- Link the guide from the README so users can find it.
- Use current CLI help, implementation behavior, and tests as the source of truth; explicitly state that `factory run` queues a request and the current local controller processes human actions but does not yet orchestrate the complete request-to-delivery pipeline.
- Avoid adding a documentation site generator, runtime dependency, hosted service, or provider configuration.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

None.

This is a documentation-only change with no system behavior requirements.

The change declares `skip_specs: true` in `.openspec.yaml` rather than inventing a behavior spec.

## Impact

The change adds `docs/factory-guide.md` and a discovery link in `README.md`.

It documents existing commands and Python APIs without changing their behavior.

Assumption: the Markdown page is the requested web page and will be rendered by the repository's hosting interface, because the project has no existing `docs/` directory or documentation-site framework.
