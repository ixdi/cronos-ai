# Proposal

## Why

The current user guide is published as Markdown, but the requested deliverable is a directly viewable HTML documentation page with its own styling.
Converting the guide makes the documentation usable as a polished page without relying on a Markdown-rendering host.

## What Changes

- Convert `docs/factory-guide.md` into a styled `docs/factory-guide.html` page while preserving the guide's accurate instructions, warnings, links, and workflow explanation.
- Include responsive, accessible styling in the HTML page itself and avoid introducing a documentation framework or runtime dependency.
- Update the README discovery link to point to the HTML page.
- Preserve the existing integration-boundary explanation and render the workflow diagram as part of the page without relying on external resources.
- Remove the Markdown guide after its content has been migrated; the HTML file becomes the canonical user-facing guide.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

None.

This is a documentation-only change with no system behavior requirements, so the change opts out of specs with `skip_specs: true`.

## Impact

The change affects `docs/factory-guide.md`, the new `docs/factory-guide.html`, and the guide link in `README.md`.
It adds no application behavior, APIs, or dependencies.
