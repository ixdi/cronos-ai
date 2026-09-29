# Proposal

## Why

The README currently opens with a title and text but does not show the supplied Cronos AI banner.
Displaying the banner near the title will make the project identity immediately recognizable to repository visitors.

## What Changes

- Add an accessible Markdown image reference to `assets/banner.jpeg` directly below the `# Cronos AI` heading in `README.md`.
- Use descriptive alternative text identifying the Cronos AI banner.
- Keep the supplied image unchanged and do not copy or move the separate root-level `banner.jpeg`.
- This is a presentation-only documentation change and does not alter runtime behavior, dependencies, or public interfaces.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

None. The change only adjusts README presentation and does not change system behavior; `.openspec.yaml` declares `skip_specs: true`.

## Impact

- `README.md`
- Existing image asset `assets/banner.jpeg`
