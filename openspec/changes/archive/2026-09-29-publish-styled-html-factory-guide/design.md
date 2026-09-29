# Design

## Context

See `proposal.md` for motivation and scope.
`docs/factory-guide.md` is the current user guide and contains Markdown links, fenced shell examples, a Mermaid flowchart, and warnings about the current CLI integration boundary.
`README.md` links directly to that Markdown file.
There is no documentation-site framework or existing build pipeline for this guide.

## Goals / Non-Goals

**Goals:**

- Keep the guide directly usable from a local checkout or static repository browser.
- Preserve user-facing information and working relative links while giving the page a clear, responsive visual hierarchy.
- Make the workflow diagram understandable without a JavaScript or network dependency.

**Non-Goals:**

- Add a site generator, JavaScript bundle, hosted documentation service, or build-time conversion step.
- Rewrite the documented product behavior or expand the user guide's subject matter.

## Decisions

- Deliver one semantic HTML document at `docs/factory-guide.html` with embedded CSS. This keeps styling and content together and lets users open the file directly. A separate stylesheet would add another asset and path-management requirement without improving this single-page use case.
- Use standard HTML elements for headings, navigation, lists, code samples, tables, and links. Add a skip link, descriptive page title, visible focus states, sufficient contrast, and responsive layout. Respect reduced-motion preferences and avoid decorative animation.
- Replace the fenced Mermaid block with a self-contained inline SVG workflow illustration, including an accessible title and description. This avoids depending on Mermaid support in a hosting interface or loading third-party scripts. Preserve the diagram's distinction between available CLI behavior and API workflow orchestration.
- Convert Markdown links to relative HTML links or in-page anchors as appropriate. Keep links to repository files such as `README.md`, `factory.md`, and `ai_factory_workflow.mmd` usable from the new location.
- Make the HTML page canonical: update the README link and remove `docs/factory-guide.md` in the same change so that there is only one maintained copy of the guide.
- Do not add conversion tooling or dependencies. Validate the page as static HTML, check internal and relative links, and review its narrow and wide layouts in a browser.

Alternatives considered: retain Markdown as the canonical source and generate HTML, which introduces synchronization or build-tool requirements; keep both Markdown and HTML hand-maintained, which risks content drift; use a remote Mermaid renderer, which makes the diagram unavailable offline and adds a third-party runtime dependency.

## Risks / Trade-offs

- [Manual conversion can omit or alter guide details] → Compare the HTML content section by section with the current Markdown before deleting it, and verify shell commands, warnings, and links.
- [Inline SVG can be difficult to read on small screens] → Provide a horizontally scrollable diagram container with an accessible text summary and keep the rest of the page responsive.
- [Removing the Markdown path can break existing external links] → Update the repository's README link and search repository references before removal; the migration intentionally makes the HTML path canonical.

## Migration Plan

1. Convert the guide content to `docs/factory-guide.html` with embedded styles and a self-contained accessible diagram.
2. Update `README.md` to link to the HTML page and update any other in-repository references.
3. Remove `docs/factory-guide.md` after confirming the migrated content and links.
4. Validate markup, relative links, and responsive rendering. Roll back by reverting the documentation change if the page is not usable; no application data or runtime migration is involved.
