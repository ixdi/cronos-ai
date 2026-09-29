from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
from pydantic import ValidationError

from cronos_ai.models import (
    Attempt,
    AttemptStatus,
    FactoryConfig,
    IntegrationOutcome,
    IntegrationResult,
    OpenSpecPlan,
    PlanTask,
    RequestSource,
    SpecialistProfile,
    TaskRecord,
    TaskState,
    TriageOutcome,
    WorkerSlot,
    WorkerStatus,
    WorkRequest,
)


def test_factory_config_rejects_non_positive_limits() -> None:
    with pytest.raises(ValidationError):
        FactoryConfig(state_dir=Path(".factory"), max_concurrency=0)

    with pytest.raises(ValidationError):
        FactoryConfig(state_dir=Path(".factory"), max_attempts=0)


def test_factory_config_rejects_unknown_settings() -> None:
    with pytest.raises(ValidationError):
        FactoryConfig(state_dir=Path(".factory"), unsafe_fallback=True)


def test_work_request_requires_description_and_explicit_repository() -> None:
    with pytest.raises(ValidationError):
        WorkRequest(request_id="request-1", description=" ", repo_path=Path("repo"))

    with pytest.raises(ValidationError):
        WorkRequest(request_id="request-1", description="Fix issue")


def test_plan_rejects_missing_dependencies_and_cycles() -> None:
    with pytest.raises(ValidationError):
        OpenSpecPlan(
            change_name="valid-change",
            tasks=(
                PlanTask(task_id="one", description="Task", depends_on=("missing",)),
            ),
        )

    with pytest.raises(ValidationError):
        OpenSpecPlan(
            change_name="valid-change",
            tasks=(
                PlanTask(task_id="one", description="One", depends_on=("two",)),
                PlanTask(task_id="two", description="Two", depends_on=("one",)),
            ),
        )


def test_task_state_rejects_unknown_values() -> None:
    with pytest.raises(ValidationError):
        TaskRecord(task_id="one", state="completed")

    assert TaskState.QUEUED.value == "queued"
    assert TaskState.WAITING_FOR_HUMAN.value == "waiting for human"


def test_specialist_profile_requires_explicit_runtime_configuration() -> None:
    with pytest.raises(ValidationError):
        SpecialistProfile(
            name="implementer",
            role="Implementation",
            model="",
            provider="provider",
            provider_api_key_env="FACTORY_PROVIDER_KEY",
        )

    with pytest.raises(ValidationError):
        SpecialistProfile(
            name="implementer",
            role="Implementation",
            model="model-a",
            provider="provider",
            provider_api_key_env="FACTORY_PROVIDER_KEY",
            inherit_host_tools=True,
        )


def test_specialist_egress_allowlist_accepts_only_https_origins() -> None:
    base_profile = {
        "name": "implementer",
        "role": "Implementation",
        "model": "model-a",
        "provider": "provider-a",
        "provider_api_key_env": "FACTORY_PROVIDER_KEY",
    }

    with pytest.raises(ValidationError, match="HTTPS origins"):
        SpecialistProfile(
            **base_profile,
            network_allowlist=("http://api.provider.example",),
        )
    with pytest.raises(ValidationError, match="HTTPS origins"):
        SpecialistProfile(
            **base_profile,
            network_allowlist=("https://api.provider.example/private",),
        )
    with pytest.raises(ValidationError, match="HTTPS origins"):
        SpecialistProfile(
            **base_profile,
            network_allowlist=("https://user:password@api.provider.example",),
        )


def test_worker_slot_requires_task_for_working_state() -> None:
    with pytest.raises(ValidationError):
        WorkerSlot(
            worker_id="worker-1",
            session_id="session-1",
            pane_id="pane-1",
            status=WorkerStatus.WORKING,
        )


def test_attempt_rejects_invalid_lifecycle_and_failure_without_reason() -> None:
    started = datetime.now(UTC)

    with pytest.raises(ValidationError):
        Attempt(
            task_id="task-1",
            attempt_number=1,
            status=AttemptStatus.RUNNING,
            started_at=started,
            finished_at=started + timedelta(seconds=1),
        )

    with pytest.raises(ValidationError):
        Attempt(
            task_id="task-1",
            attempt_number=1,
            status=AttemptStatus.FAILED,
            started_at=started,
            finished_at=started + timedelta(seconds=1),
        )


def test_integration_result_cannot_claim_success_without_reference() -> None:
    with pytest.raises(ValidationError):
        IntegrationResult(
            integration="ci",
            outcome=IntegrationOutcome.SUCCEEDED,
        )


def test_valid_domain_models_accept_known_values() -> None:
    request = WorkRequest(
        request_id="request-1",
        description="Fix the login flow",
        repo_path=Path("/workspace/project"),
        source=RequestSource.USER,
        triage_outcome=TriageOutcome.ACTIONABLE,
    )
    plan = OpenSpecPlan(
        change_name="fix-login-flow",
        tasks=(
            PlanTask(task_id="one", description="Implement fix"),
            PlanTask(task_id="two", description="Verify fix", depends_on=("one",)),
        ),
    )
    profile = SpecialistProfile(
        name="implementer",
        role="Implementation",
        model="model-a",
        provider="provider-a",
        provider_api_key_env="FACTORY_PROVIDER_KEY",
        skills=("python",),
        mcp_tools=("git",),
        network_allowlist=("https://api.provider.example",),
    )
    now = datetime.now(UTC)
    attempt = Attempt(
        task_id="one",
        attempt_number=1,
        status=AttemptStatus.SUCCEEDED,
        started_at=now,
        finished_at=now + timedelta(seconds=1),
    )
    result = IntegrationResult(
        integration="ci",
        outcome=IntegrationOutcome.SUCCEEDED,
        external_id="run-42",
    )

    assert request.triage_outcome is TriageOutcome.ACTIONABLE
    assert plan.tasks[1].depends_on == ("one",)
    assert profile.skills == ("python",)
    assert attempt.status is AttemptStatus.SUCCEEDED
    assert result.external_id == "run-42"
