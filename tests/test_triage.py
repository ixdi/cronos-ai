import pytest
from pydantic import ValidationError

from cronos_ai.models import TriageOutcome, WorkRequest
from cronos_ai.triage import TriageResult, triage_request


def make_request() -> WorkRequest:
    return WorkRequest(
        request_id="request-1",
        description="Improve the settings page",
        repo_path="/workspace/project",
    )


def test_actionable_work_can_be_planned_and_dispatched() -> None:
    result = triage_request(
        make_request(),
        TriageOutcome.ACTIONABLE,
        rationale="The change is clear and bounded.",
    )

    assert result.request.triage_outcome is TriageOutcome.ACTIONABLE
    assert result.can_plan is True
    assert result.can_dispatch_directly is True


def test_specifications_required_work_can_be_planned_but_not_dispatched() -> None:
    result = triage_request(
        make_request(),
        TriageOutcome.SPECS_REQUIRED,
        rationale="The request needs fuller product and technical specifications.",
    )

    assert result.can_plan is True
    assert result.can_dispatch_directly is False


def test_clarification_required_work_waits_for_answers() -> None:
    result = triage_request(
        make_request(),
        TriageOutcome.CLARIFICATION_REQUIRED,
        rationale="The expected behavior is unclear.",
        questions=("Should the setting apply to all users?",),
    )

    assert result.can_plan is False
    assert result.can_dispatch_directly is False
    assert result.questions == ("Should the setting apply to all users?",)


def test_parked_work_cannot_advance() -> None:
    result = triage_request(
        make_request(),
        TriageOutcome.PARKED,
        rationale="The request is deferred by the requester.",
    )

    assert result.can_plan is False
    assert result.can_dispatch_directly is False


def test_clarification_requires_questions() -> None:
    with pytest.raises(ValidationError, match="clarification requires at least one"):
        TriageResult(
            request=make_request(),
            outcome=TriageOutcome.CLARIFICATION_REQUIRED,
            rationale="A decision is missing.",
        )


def test_non_clarification_outcomes_cannot_carry_blocking_questions() -> None:
    with pytest.raises(ValidationError, match="only valid for clarification"):
        TriageResult(
            request=make_request(),
            outcome=TriageOutcome.ACTIONABLE,
            rationale="The request is clear.",
            questions=("Should we proceed?",),
        )


def test_triage_does_not_mutate_original_request() -> None:
    request = make_request()

    triage_request(
        request,
        TriageOutcome.PARKED,
        rationale="Deferred.",
    )

    assert request.triage_outcome is None
