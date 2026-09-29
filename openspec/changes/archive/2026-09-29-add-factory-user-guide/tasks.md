# Tasks

## 1. User guide

- [x] 1.1 Create `docs/factory-guide.md` with prerequisites, a first-use path, CLI usage, human approvals, recovery, and links to deeper setup; verify every documented CLI command against `factory --help` and its implementation.
- [x] 1.2 Explain worker, sandbox, webhook, review, and CI/CD APIs and clearly distinguish them from the CLI/controller behavior currently wired end to end; verify all capability and limitation statements against the README, source, and tests.

## 2. Workflow diagram and discoverability

- [x] 2.1 Add an embedded Mermaid flowchart that shows request intake, triage, planning approval, dependency scheduling, worker execution, integration, review, delivery, and webhook feedback; verify the diagram identifies which transitions are CLI-driven and which currently require API orchestration.
- [x] 2.2 Link the new guide from `README.md` and verify its relative link and Mermaid block are present and valid without adding a site framework or runtime dependency.
