# Design

## Context

The project currently has a detailed README, `factory.md`, and a root Mermaid workflow source, but no `docs/` directory or documentation-site framework.

The CLI validates and durably queues explicit repository requests, while the local controller currently processes human actions and maintains its lifecycle rather than orchestrating the entire request-to-delivery pipeline.

The planning, scheduling, worker, review, webhook, and CI/CD components are available through Python APIs and are exercised together by an end-to-end test.

The guide must describe that distinction so users do not mistake an architectural workflow diagram for an already automated CLI command.

## Goals / Non-Goals

**Goals:**

- Provide one discoverable, task-oriented guide for first use and further learning.
- Explain prerequisites, safe repository intake, planning and approval, human attention, worker execution, review, delivery, and webhook boundaries.
- Include a Mermaid flowchart that distinguishes CLI behavior from the component workflow that currently requires programmatic orchestration.
- Keep all guidance grounded in the current README, CLI help, implementation, and tests.

**Non-Goals:**

- Add or deploy a website, documentation generator, JavaScript runtime, CDN dependency, or hosted service.
- Change the CLI, controller, OpenSpec planning behavior, or execution APIs.
- Present the complete end-to-end component workflow as an automatic `factory run` behavior.
- Replace the existing README or root-level architecture material.

## Decisions

### Use a repository-rendered Markdown page

Create `docs/factory-guide.md` and link it from the README.

This matches the repository's existing Markdown documentation and avoids introducing a build or runtime dependency.

The assumption is that the repository host renders Markdown and Mermaid blocks as a web page.

A standalone HTML page or a full documentation site was considered, but neither has an existing project framework to build on.

### Keep the workflow diagram in Mermaid source

Embed a fenced `mermaid` flowchart in the guide rather than generating an image or loading a browser-side Mermaid package.

The diagram will show request intake, triage, planning and approval, dependency scheduling, worker execution, integration, review, and delivery.

It will separately identify the CLI/controller boundary and the API-composed pipeline to make present automation limits visible.

The existing root `ai_factory_workflow.mmd` is useful product context, but the guide diagram must reflect verified runtime behavior rather than copy aspirational claims unchanged.

### Use the existing README and implementation as documentation sources

The guide will link to relevant README sections and use exact CLI subcommands and option names from the parser.

It will state the explicit clean-repository/OpenSpec prerequisites and will not imply that webhook content executes commands or bypasses triage and approval.

It will identify provider integrations that require explicit configuration and note that no hosted relay or default CI/CD provider is supplied.

### Keep this change documentation-only

No behavior requirements change, so the change declares `skip_specs: true` in `.openspec.yaml`.

The implementation tasks will include link, command, diagram-source, and accuracy checks.

## Risks / Trade-offs

- Repository hosts differ in Mermaid rendering support. The Mermaid source remains readable even when a host displays it as code rather than a rendered diagram.
- The controller's current runtime scope is narrower than the target architecture. The guide will call this out prominently and will be checked against `controller.py` and the CLI before acceptance.
- A short onboarding guide could omit advanced integration setup. It will link to the detailed README sections instead of duplicating their configuration examples.
