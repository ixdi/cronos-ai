import subprocess
from pathlib import Path

import pytest

from cronos_ai.attention import HumanAttentionQueue
from cronos_ai.delivery import (
    DeliveryCoordinator,
    DeliveryError,
    DeliveryStatus,
    FakeCIAdapter,
)
from cronos_ai.models import (
    OpenSpecPlan,
    PlanTask,
    RunContext,
    TaskRecord,
    TaskState,
    UserScenarioCheck,
    VerificationCheck,
    WorkRequest,
)
from cronos_ai.review import ReviewCoordinator
from cronos_ai.storage import FactoryStore


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def commit(repository: Path, message: str) -> None:
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "user.name=delivery-test",
            "-c",
            "user.email=delivery-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def create_approved_run(tmp_path: Path) -> tuple[FactoryStore, str]:
    repository = tmp_path / "repository"
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(repository, "config", "user.name", "delivery-test")
    git(repository, "config", "user.email", "delivery-test@example.invalid")
    (repository / "README.md").write_text("base\n")
    git(repository, "add", "README.md")
    commit(repository, "initial")
    starting_commit = git(repository, "rev-parse", "HEAD")
    branch_name = "factory/run/delivery"
    git(repository, "branch", branch_name)
    worktree = tmp_path / "run-worktree"
    git(repository, "worktree", "add", str(worktree), branch_name)
    (worktree / "feature.txt").write_text("delivered feature\n")
    git(worktree, "add", "feature.txt")
    commit(worktree, "implement feature")

    request = WorkRequest(
        request_id="request-delivery",
        description="Implement and deliver a feature",
        repo_path=repository,
    )
    plan = OpenSpecPlan(
        change_name="deliver-feature",
        tasks=(PlanTask(task_id="implement", description="Implement feature"),),
    )
    store = FactoryStore(tmp_path / "factory.sqlite3")
    store.create_run(
        "run-delivery",
        request,
        plan,
        (TaskRecord(task_id="implement", state=TaskState.REVIEW),),
    )
    store.save_run_context(
        RunContext(
            run_id="run-delivery",
            branch_name=branch_name,
            run_worktree_path=worktree,
            starting_commit=starting_commit,
        )
    )
    checks = (
        VerificationCheck(
            name="test suite",
            command="uv run pytest",
            passed=True,
            summary="All tests passed.",
        ),
    )
    scenarios = (
        UserScenarioCheck(
            scenario="The user can complete the feature workflow",
            passed=True,
            evidence="The workflow completed successfully.",
        ),
    )
    reviews = ReviewCoordinator(store)
    packet = reviews.prepare_review(
        "run-delivery",
        review_summary="Implementation and evidence are ready.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    reviews.decide_review(
        "run-delivery",
        review_hash=packet.review_hash,
        reviewer="reviewer-1",
        approve=True,
    )
    assert reviews.can_deliver("run-delivery")
    return store, "run-delivery"


def test_missing_ci_configuration_never_creates_successful_delivery(
    tmp_path: Path,
) -> None:
    store, run_id = create_approved_run(tmp_path)
    coordinator = DeliveryCoordinator(store, adapter=None)

    with pytest.raises(DeliveryError, match="not configured"):
        coordinator.dispatch(run_id)

    assert store.get_delivery_record(run_id) is None
    assert not coordinator.is_delivered(run_id)
    store.close()


def test_fake_adapter_success_requires_approved_review_and_records_delivery(
    tmp_path: Path,
) -> None:
    store, run_id = create_approved_run(tmp_path)
    adapter = FakeCIAdapter(dispatch_status=DeliveryStatus.SUCCEEDED)
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    record = coordinator.dispatch(run_id)

    assert record.status is DeliveryStatus.SUCCEEDED
    assert record.external_id is not None
    assert adapter.dispatch_calls == 1
    assert coordinator.is_delivered(run_id)
    assert coordinator.dispatch(run_id) == record
    assert adapter.dispatch_calls == 1
    store.close()


def test_ci_dispatch_is_blocked_until_the_latest_review_is_approved(
    tmp_path: Path,
) -> None:
    store, run_id = create_approved_run(tmp_path)
    adapter = FakeCIAdapter(dispatch_status=DeliveryStatus.SUCCEEDED)
    checks = (
        VerificationCheck(
            name="test suite",
            command="uv run pytest",
            passed=True,
            summary="All tests passed.",
        ),
    )
    scenarios = (
        UserScenarioCheck(
            scenario="The workflow completes",
            passed=True,
            evidence="The workflow completed.",
        ),
    )
    ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="A newer review packet is awaiting approval.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    with pytest.raises(DeliveryError, match="human-approved review"):
        coordinator.dispatch(run_id)

    assert adapter.dispatch_calls == 0
    assert store.get_delivery_record(run_id) is None
    assert not coordinator.is_delivered(run_id)
    store.close()


def test_changed_worktree_invalidates_delivery_approval(tmp_path: Path) -> None:
    store, run_id = create_approved_run(tmp_path)
    context = store.get_run_context(run_id)
    assert context is not None
    (context.run_worktree_path / "feature.txt").write_text("unreviewed edit\n")
    adapter = FakeCIAdapter(dispatch_status=DeliveryStatus.SUCCEEDED)
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    with pytest.raises(DeliveryError, match="human-approved review"):
        coordinator.dispatch(run_id)

    assert adapter.dispatch_calls == 0
    assert not coordinator.is_delivered(run_id)
    store.close()


def test_inconclusive_ci_result_never_reports_delivery_success(tmp_path: Path) -> None:
    store, run_id = create_approved_run(tmp_path)
    adapter = FakeCIAdapter(dispatch_status=DeliveryStatus.INCONCLUSIVE)
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    record = coordinator.dispatch(run_id)

    assert record.status is DeliveryStatus.INCONCLUSIVE
    assert not coordinator.is_delivered(run_id)
    store.close()


def test_running_ci_workflow_becomes_delivered_only_after_success(
    tmp_path: Path,
) -> None:
    store, run_id = create_approved_run(tmp_path)
    adapter = FakeCIAdapter(
        dispatch_status=DeliveryStatus.RUNNING,
        status_results=(DeliveryStatus.SUCCEEDED,),
    )
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    dispatched = coordinator.dispatch(run_id)
    assert dispatched.status is DeliveryStatus.RUNNING
    assert not coordinator.is_delivered(run_id)

    completed = coordinator.refresh_status(run_id)

    assert completed.status is DeliveryStatus.SUCCEEDED
    assert coordinator.is_delivered(run_id)
    assert adapter.dispatch_calls == 1
    assert adapter.status_calls == 1
    store.close()


def test_failed_ci_workflow_never_reports_delivery_success(tmp_path: Path) -> None:
    store, run_id = create_approved_run(tmp_path)
    adapter = FakeCIAdapter(dispatch_status=DeliveryStatus.FAILED)
    coordinator = DeliveryCoordinator(store, adapter=adapter)

    record = coordinator.dispatch(run_id)

    assert record.status is DeliveryStatus.FAILED
    assert not coordinator.is_delivered(run_id)
    item = next(
        item
        for item in HumanAttentionQueue(store).list_items(run_id)
        if item.item_id == f"{run_id}:delivery"
    )
    assert item.available_actions == ()
    assert "Inspect the CI/CD workflow" in (item.recommended_action or "")
    store.close()
