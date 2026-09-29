"""Plan approval validation and implementation dispatch gates."""

from __future__ import annotations

import hashlib
from pathlib import Path

from cronos_ai.models import (
    ApprovalDecision,
    PlanApproval,
    TriageResult,
)


class ApprovalError(ValueError):
    """Raised when a plan cannot be fingerprinted for human approval."""


def plan_fingerprint(change_dir: Path) -> str:
    """Return a stable SHA-256 fingerprint of all files in a plan change."""
    if not change_dir.is_dir():
        raise ApprovalError(f"OpenSpec change directory does not exist: {change_dir}")

    files = sorted(change_dir.rglob("*"))
    digest = hashlib.sha256()
    file_count = 0
    for path in files:
        if path.is_symlink():
            raise ApprovalError("OpenSpec plan must not contain symbolic links")
        if not path.is_file():
            continue
        relative_path = path.relative_to(change_dir).as_posix()
        try:
            content = path.read_bytes()
        except OSError as error:
            raise ApprovalError(
                f"could not read plan artifact: {relative_path}"
            ) from error
        digest.update(relative_path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(content)
        digest.update(b"\0")
        file_count += 1

    if file_count == 0:
        raise ApprovalError("OpenSpec plan has no files to approve")
    return digest.hexdigest()


def can_dispatch_implementation(
    triage: TriageResult,
    *,
    change_name: str,
    plan_hash: str,
    approval: PlanApproval | None,
) -> bool:
    """Return whether triage and any required approval authorize dispatch."""
    if not triage.can_plan:
        return False
    if not triage.requires_approval:
        return triage.can_dispatch_directly
    return (
        approval is not None
        and approval.change_name == change_name
        and approval.plan_hash == plan_hash
        and approval.decision is ApprovalDecision.APPROVED
    )
