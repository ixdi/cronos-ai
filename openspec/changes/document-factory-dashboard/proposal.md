# Proposal

## Why

The repository README now documents the terminal dashboard, but the published user guide at `docs/index.html` does not expose it in its navigation or walkthrough. Users following the guide therefore miss how to monitor active factories, page through safe activity, and inspect outputs without changing run state.

## What Changes

- Expand the README dashboard guidance and add a matching, navigable dashboard section to the published HTML user guide.
- Document `cronos-ai dashboard`, the `--state-dir` override, interactive-terminal requirement, run and task selection, refresh and activity-page keys, and the meaning of completed-task percentages.
- Explain the read-only boundary, how to continue human decisions through existing `cronos-ai action` commands, which output paths are shown, and the limits of activity redaction and tool-payload omission.
- Keep both documents aligned with current CLI help and implemented dashboard behavior; do not describe future orchestration as available.

## Capabilities

### New Capabilities

None. This change is documentation-only and does not introduce system behavior.

### Modified Capabilities

None. The existing dashboard behavior is unchanged.

This change opts out of spec deltas with `skip_specs: true` in its `.openspec.yaml`.

## Impact

`README.md` and `docs/index.html`, including the website table of contents and in-page dashboard guidance. Documentation checks should cover navigation anchors, command and option parity with CLI help, and the documented interaction/safety claims. No application code, APIs, dependencies, or runtime behavior are in scope.
