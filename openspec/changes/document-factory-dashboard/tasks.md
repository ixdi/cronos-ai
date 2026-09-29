# Tasks

## 1. Documentation regression coverage

- [ ] 1.1 Add `tests/test_dashboard_docs.py` checks using Python's standard-library HTML parser for a dashboard table-of-contents link with a matching section ID, the documented command and state override, keyboard controls, read-only boundary, progress definition, and activity-safety caveat; verify the new guide-content assertions fail before editing `docs/index.html`.
- [ ] 1.2 Add checks that documented CLI examples and option names exist in `uv run cronos-ai dashboard --help`; verify the tests pass for the current `dashboard` command and `--state-dir` option.

## 2. Update the repository README and published guide

- [ ] 2.1 Add a dashboard entry to the published guide navigation and an accessible `#dashboard` section after the controller guidance; cover launch, custom state, terminal requirement, run/task selection, refresh and activity paging keys, completed-task percentage, outputs, read-only actions, and safe activity summaries; verify `uv run pytest tests/test_dashboard_docs.py -q` passes.
- [ ] 2.2 Expand the README dashboard section only where it adds information beyond the existing quick-start description, and link to the published guide's dashboard section; verify the documentation tests confirm both sources describe the same current command, controls, and safety boundary.

## 3. Published-page verification

- [ ] 3.1 Serve `docs/index.html` locally and use Chrome DevTools to verify the dashboard navigation anchor works, the new section is legible at desktop and narrow viewport widths, keyboard focus reaches the navigation and command examples, and the page has no new console errors; verify the section remains usable without horizontal page overflow.
- [ ] 3.2 Run `uv run pytest tests/test_dashboard_docs.py -q` and `uv run cronos-ai dashboard --help`, then review `git diff --check` and `git status --short` to confirm only the planned README, published guide, documentation tests, and OpenSpec artifacts changed.
