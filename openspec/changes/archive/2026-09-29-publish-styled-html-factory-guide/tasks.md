# Tasks

## 1. Convert the guide to self-contained HTML

- [x] 1.1 Create `docs/factory-guide.html` with semantic document structure and embedded responsive CSS, including accessible navigation, keyboard focus styles, contrast, and reduced-motion support; verify it opens as a standalone HTML page and inspect narrow and wide layouts in a browser.
- [x] 1.2 Migrate every guide section, command example, warning, table, and repository link from `docs/factory-guide.md`; replace the Mermaid fence with an accessible inline SVG showing the existing CLI/API boundaries; verify the content against the Markdown source and check all relative links.

## 2. Switch documentation discovery to HTML

- [x] 2.1 Update the README guide link to `docs/factory-guide.html`, search the repository for remaining references to `docs/factory-guide.md`, and remove the Markdown file only after checking its content is migrated; verify the README link resolves and no in-repository links target the removed path.

## 3. Validate the published page

- [x] 3.1 Validate the final HTML structure and page links, then review the page at desktop and mobile widths with keyboard navigation; verify there are no missing guide sections, broken links, browser console errors, or inaccessible diagram descriptions.
