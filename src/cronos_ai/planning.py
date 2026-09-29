"""Generate validated OpenSpec drafts from triaged factory requests."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

from cronos_ai.approval import plan_fingerprint
from cronos_ai.models import (
    OpenSpecPlan,
    PlanTask,
    PlanTaskKind,
    TriageResult,
    WorkRequest,
)


class PlanningError(RuntimeError):
    """Raised when an OpenSpec plan cannot safely be generated."""


@dataclass(frozen=True)
class GeneratedOpenSpecChange:
    """A validated OpenSpec draft and its dispatch eligibility."""

    plan: OpenSpecPlan
    change_dir: Path
    full_plan: bool
    can_dispatch_directly: bool
    plan_hash: str

    @property
    def change_name(self) -> str:
        return self.plan.change_name

    @property
    def tasks(self) -> tuple[PlanTask, ...]:
        return self.plan.tasks


def _slug(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.casefold()).strip("-")


def _change_name(request: WorkRequest) -> str:
    title = _slug(request.description)[:40].rstrip("-") or "request"
    identifier = _slug(request.request_id)[-20:] or "new"
    return f"{title}-{identifier}"


def _fenced_text(value: str) -> str:
    longest_backticks = max(
        (len(match.group()) for match in re.finditer(r"`+", value)),
        default=0,
    )
    fence = "`" * max(3, longest_backticks + 1)
    return f"{fence}text\n{value}\n{fence}"


def _artifacts(
    request: WorkRequest,
    triage: TriageResult,
    change_name: str,
) -> tuple[dict[str, str], tuple[PlanTask, ...]]:
    full_plan = not triage.can_dispatch_directly
    request_text = _fenced_text(request.description)
    rationale = _fenced_text(triage.rationale)
    tasks: tuple[PlanTask, ...]

    proposal = f"""# Proposal: {change_name}

## Why

This change addresses the submitted request:

{request_text}

## What Changes

- Implement and verify the requested outcome recorded above.
- {
        "Develop detailed product and technical specifications before implementation."
        if full_plan
        else "Keep implementation scoped to the bounded request."
    }

## Capabilities

### New Capabilities

- `requested-outcome`: Deliver the behavior described by the accepted request.

### Modified Capabilities

None.

## Impact

- Target repository behavior and its relevant tests.
"""

    spec = """# Spec Delta

## Purpose

Describes the behavior requested for this change.

## ADDED Requirements

### Requirement: Fulfill the accepted request
The implementation SHALL satisfy the submitted request recorded in the proposal.

#### Scenario: The requested outcome is delivered
- **WHEN** implementation for this change is complete
- **THEN** the requested outcome is available and its behavior is verified
"""

    if full_plan:
        design = f"""# Detailed Planning Draft

## Request

{request_text}

## Triage Rationale

{rationale}

## Planning Requirements

- Refine the expected behavior and acceptance criteria before implementation.
- Record design decisions and dependencies in this change.
- Obtain explicit human approval before implementation is dispatched.
- Treat unresolved product decisions as clarification blockers.
"""
        tasks = (
            PlanTask(
                task_id="1.1",
                description=(
                    "Refine product behavior and acceptance criteria from the request."
                ),
                kind=PlanTaskKind.PLANNING,
            ),
            PlanTask(
                task_id="1.2",
                description="Record technical design and implementation dependencies.",
                depends_on=("1.1",),
                kind=PlanTaskKind.PLANNING,
            ),
            PlanTask(
                task_id="1.3",
                description="Obtain human approval of the detailed OpenSpec plan.",
                depends_on=("1.2",),
                kind=PlanTaskKind.APPROVAL,
            ),
            PlanTask(
                task_id="2.1",
                description="Implement the approved behavior in the target repository.",
                depends_on=("1.3",),
                kind=PlanTaskKind.IMPLEMENTATION,
            ),
            PlanTask(
                task_id="2.2",
                description=(
                    "Verify requested behavior and relevant regression tests."
                ),
                depends_on=("2.1",),
                kind=PlanTaskKind.VERIFICATION,
            ),
        )
        task_lines = """## 1. Detailed planning and approval

- [ ] 1.1 Refine product behavior and acceptance criteria from the request.
- [ ] 1.2 Record technical design and implementation dependencies.
- [ ] 1.3 Obtain human approval of the detailed OpenSpec plan.

## 2. Implementation and verification

- [ ] 2.1 Implement the approved behavior in the target repository.
- [ ] 2.2 Verify the requested behavior and relevant regression tests.
"""
    else:
        design = """# Design

## Approach

Implement the bounded request in the target repository without expanding its scope.

## Verification

Run the relevant tests and verify the requested behavior from the user perspective.
"""
        tasks = (
            PlanTask(
                task_id="1.1",
                description="Implement the behavior described in the accepted request.",
            ),
            PlanTask(
                task_id="1.2",
                description=(
                    "Verify requested behavior and relevant regression tests."
                ),
                depends_on=("1.1",),
                kind=PlanTaskKind.VERIFICATION,
            ),
        )
        task_lines = """## 1. Implementation and verification

- [ ] 1.1 Implement the behavior described in the accepted request.
- [ ] 1.2 Verify the requested behavior and relevant regression tests.
"""

    task_document = f"""# Tasks

{task_lines}
"""
    return (
        {
            "proposal.md": proposal,
            "specs/requested-outcome/spec.md": spec,
            "design.md": design,
            "tasks.md": task_document,
        },
        tasks,
    )


def generate_openspec_change(
    request: WorkRequest,
    triage: TriageResult,
) -> GeneratedOpenSpecChange:
    """Create and validate a concise or detailed OpenSpec change draft."""
    if not triage.can_plan:
        raise PlanningError("this triage outcome cannot be planned")
    if (
        triage.request.request_id != request.request_id
        or triage.request.repo_path != request.repo_path
    ):
        raise PlanningError("triage result does not match the submitted request")

    repository = request.repo_path.resolve()
    openspec_root = repository / "openspec"
    if not repository.is_dir() or not any(
        (openspec_root / config_name).is_file()
        for config_name in ("config.yaml", "config.yml")
    ):
        raise PlanningError(
            "target repository does not have an initialized OpenSpec root"
        )

    change_name = _change_name(request)
    change_dir = openspec_root / "changes" / change_name
    if change_dir.exists():
        raise PlanningError(f"OpenSpec change already exists: {change_name}")

    artifacts, tasks = _artifacts(request, triage, change_name)
    plan = OpenSpecPlan(change_name=change_name, tasks=tasks)
    create_command = [
        "openspec",
        "new",
        "change",
        change_name,
        "--schema",
        "spec-driven",
    ]
    try:
        result = subprocess.run(
            create_command,
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise PlanningError("OpenSpec CLI is unavailable") from error
    if result.returncode != 0:
        details = result.stderr.strip() or result.stdout.strip()
        message = f"OpenSpec change creation failed with exit code {result.returncode}"
        if details:
            message = f"{message}: {details}"
        raise PlanningError(message)
    if not change_dir.is_dir():
        raise PlanningError(
            "OpenSpec CLI did not create the requested change directory"
        )

    try:
        for relative_path, content in artifacts.items():
            artifact_path = change_dir / relative_path
            if artifact_path.exists():
                raise PlanningError(
                    f"OpenSpec CLI created an unexpected artifact: {relative_path}"
                )
            artifact_path.parent.mkdir(parents=True, exist_ok=True)
            artifact_path.write_text(content, encoding="utf-8")
    except OSError as error:
        raise PlanningError(f"could not write OpenSpec artifacts: {error}") from error

    validate_command = [
        "openspec",
        "validate",
        change_name,
        "--no-interactive",
    ]
    try:
        validation = subprocess.run(
            validate_command,
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise PlanningError("OpenSpec validation could not be run") from error
    if validation.returncode != 0:
        details = validation.stdout.strip() or validation.stderr.strip()
        message = f"generated OpenSpec change failed validation: {change_name}"
        if details:
            message = f"{message}: {details}"
        raise PlanningError(message)

    return GeneratedOpenSpecChange(
        plan=plan,
        change_dir=change_dir,
        full_plan=not triage.can_dispatch_directly,
        can_dispatch_directly=triage.can_dispatch_directly,
        plan_hash=plan_fingerprint(change_dir),
    )
