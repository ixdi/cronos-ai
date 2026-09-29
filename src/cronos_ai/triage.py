"""Request triage and safe workflow progression."""

from cronos_ai.models import (
    PlanRisk,
    TriageOutcome,
    TriageResult,
    WorkRequest,
)


def triage_request(
    request: WorkRequest,
    outcome: TriageOutcome,
    *,
    rationale: str,
    questions: tuple[str, ...] = (),
    risks: tuple[PlanRisk, ...] = (),
) -> TriageResult:
    """Record a triage decision without mutating the submitted request."""
    triaged_request = request.model_copy(update={"triage_outcome": outcome})
    return TriageResult(
        request=triaged_request,
        outcome=outcome,
        rationale=rationale,
        questions=questions,
        risks=risks,
    )
