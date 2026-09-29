import pytest

from cronos_ai.approval import (
    can_dispatch_implementation,
    plan_fingerprint,
)
from cronos_ai.models import (
    ApprovalDecision,
    PlanApproval,
    PlanRisk,
    TriageOutcome,
    WorkRequest,
)
from cronos_ai.triage import triage_request

PLAN_HASH = "a" * 64


def make_request() -> WorkRequest:
    return WorkRequest(
        request_id="request-1",
        description="Change account behavior",
        repo_path="/workspace/project",
    )


def make_approval(
    *,
    decision: ApprovalDecision = ApprovalDecision.APPROVED,
    change_name: str = "change-account-behavior-request-1",
    plan_hash: str = PLAN_HASH,
    rationale: str | None = None,
) -> PlanApproval:
    return PlanApproval(
        change_name=change_name,
        plan_hash=plan_hash,
        decision=decision,
        reviewer="reviewer-1",
        rationale=rationale,
    )


@pytest.mark.parametrize(
    "risk",
    [
        PlanRisk.SUBSTANTIAL,
        PlanRisk.SECURITY,
        PlanRisk.AUTHENTICATION,
        PlanRisk.DESTRUCTIVE_MIGRATION,
        PlanRisk.PRODUCTION_INFRASTRUCTURE,
        PlanRisk.DEPLOYMENT,
    ],
)
def test_high_impact_triage_requires_approval(risk: PlanRisk) -> None:
    triage = triage_request(
        make_request(),
        TriageOutcome.ACTIONABLE,
        rationale="This change has elevated impact.",
        risks=(risk,),
    )

    assert triage.requires_approval is True
    assert triage.can_dispatch_directly is False
    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=None,
    ) is False


def test_specs_required_work_needs_matching_plan_approval() -> None:
    triage = triage_request(
        make_request(),
        TriageOutcome.SPECS_REQUIRED,
        rationale="The change is substantial.",
    )

    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=None,
    ) is False
    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=make_approval(),
    ) is True


def test_approval_for_another_change_or_plan_is_not_authoritative() -> None:
    triage = triage_request(
        make_request(),
        TriageOutcome.SPECS_REQUIRED,
        rationale="The change is substantial.",
    )

    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=make_approval(change_name="different-change"),
    ) is False
    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=make_approval(plan_hash="b" * 64),
    ) is False


def test_rejected_approval_does_not_authorize_dispatch() -> None:
    triage = triage_request(
        make_request(),
        TriageOutcome.SPECS_REQUIRED,
        rationale="The change is substantial.",
    )

    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=make_approval(
            decision=ApprovalDecision.REJECTED,
            rationale="Revise the proposal first.",
        ),
    ) is False


def test_rejected_approval_requires_a_reason() -> None:
    with pytest.raises(ValueError, match="rejected approvals require a reason"):
        make_approval(decision=ApprovalDecision.REJECTED)


def test_plan_fingerprint_changes_when_any_plan_file_changes(tmp_path) -> None:
    change_dir = tmp_path / "change"
    change_dir.mkdir()
    proposal = change_dir / "proposal.md"
    proposal.write_text("Initial proposal\n")
    first_hash = plan_fingerprint(change_dir)
    proposal.write_text("Revised proposal\n")

    assert plan_fingerprint(change_dir) != first_hash


def test_actionable_low_impact_request_can_dispatch_without_approval() -> None:
    triage = triage_request(
        make_request(),
        TriageOutcome.ACTIONABLE,
        rationale="The request is clear and bounded.",
    )

    assert can_dispatch_implementation(
        triage,
        change_name="change-account-behavior-request-1",
        plan_hash=PLAN_HASH,
        approval=None,
    ) is True
