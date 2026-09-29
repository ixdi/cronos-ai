import json
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from cronos_ai.attention import HumanActionProcessor, HumanAttentionQueue
from cronos_ai.cli import main
from cronos_ai.models import (
    ControlAction,
    ControlActionType,
    OpenSpecPlan,
    PlanTask,
    ReviewState,
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
            "user.name=review-test",
            "-c",
            "user.email=review-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def create_review_run(tmp_path: Path) -> tuple[FactoryStore, str, Path]:
    repository = tmp_path / "repository"
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(repository, "config", "user.name", "review-test")
    git(repository, "config", "user.email", "review-test@example.invalid")
    (repository / "README.md").write_text("base\n")
    git(repository, "add", "README.md")
    commit(repository, "initial")
    starting_commit = git(repository, "rev-parse", "HEAD")

    branch_name = "factory/run/review"
    git(repository, "branch", branch_name)
    run_worktree = tmp_path / "run-worktree"
    git(repository, "worktree", "add", str(run_worktree), branch_name)
    (run_worktree / "feature.py").write_text("def feature():\n    return True\n")
    git(run_worktree, "add", "feature.py")
    commit(run_worktree, "implement feature")

    request = WorkRequest(
        request_id="request-review",
        description="Implement and review a feature",
        repo_path=repository,
    )
    plan = OpenSpecPlan(
        change_name="review-feature",
        tasks=(PlanTask(task_id="implement", description="Implement feature"),),
    )
    store = FactoryStore(tmp_path / "factory.sqlite3")
    store.create_run(
        "run-review",
        request,
        plan,
        (TaskRecord(task_id="implement", state=TaskState.REVIEW),),
    )
    store.save_run_context(
        RunContext(
            run_id="run-review",
            branch_name=branch_name,
            run_worktree_path=run_worktree,
            starting_commit=starting_commit,
            task_worktree_root=tmp_path / "task-worktrees",
        )
    )
    return store, "run-review", run_worktree


def evidence() -> tuple[tuple[VerificationCheck, ...], tuple[UserScenarioCheck, ...]]:
    return (
        (
            VerificationCheck(
                name="automated tests",
                command="uv run pytest",
                passed=True,
                summary="All configured tests passed.",
            ),
        ),
        (
            UserScenarioCheck(
                scenario="A user can use the new feature",
                passed=True,
                evidence="The acceptance scenario completed successfully.",
            ),
        ),
    )


def test_review_packet_contains_integrated_diff_and_verification_evidence(
    tmp_path: Path,
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()

    packet = ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="The implementation is ready for human review.",
        review_findings=("Consider a clearer helper name.",),
        verification_checks=checks,
        user_scenarios=scenarios,
    )

    assert packet.state is ReviewState.READY
    assert "feature.py" in packet.diff_text
    assert packet.diff_hash
    assert packet.review_hash
    assert packet.review_findings == ("Consider a clearer helper name.",)
    assert packet.verification_checks == checks
    assert packet.user_scenarios == scenarios
    assert not ReviewCoordinator(store).can_deliver(run_id)
    assert store.get_latest_review_packet(run_id) == packet
    item = next(
        item
        for item in HumanAttentionQueue(store).list_items(run_id)
        if item.item_id == f"{run_id}:review"
    )
    assert item.target_id == "run"
    assert item.suggested_data["review_hash"] == packet.review_hash
    store.close()


def test_review_cli_prepares_packet_from_validated_evidence_file(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()
    store.close()
    evidence_file = tmp_path / "review-evidence.json"
    evidence_file.write_text(
        json.dumps(
            {
                "review_summary": "Code review and user checks passed.",
                "review_findings": ["The public API is concise."],
                "verification_checks": [
                    check.model_dump(mode="json") for check in checks
                ],
                "user_scenarios": [
                    scenario.model_dump(mode="json") for scenario in scenarios
                ],
            }
        ),
        encoding="utf-8",
    )

    main(
        [
            "review",
            "--run",
            run_id,
            "--evidence",
            str(evidence_file),
            "--state-dir",
            str(tmp_path),
        ]
    )

    assert "Review packet" in capsys.readouterr().out
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        packet = store.get_latest_review_packet(run_id)
    assert packet is not None and packet.review_findings == (
        "The public API is concise.",
    )


def test_review_cli_shows_integrated_diff_and_verification_results(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()
    ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="Review output is ready.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    store.close()

    main(["review", "--run", run_id, "--state-dir", str(tmp_path)])
    output = capsys.readouterr().out

    assert "Integrated diff" in output
    assert "feature.py" in output
    assert "automated tests" in output
    assert "A user can use the new feature" in output


def test_human_approval_opens_delivery_gate_for_the_exact_review_packet(
    tmp_path: Path,
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()
    packet = ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="Code review and user verification passed.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.APPROVE_REVIEW,
        target_id="run",
        run_id=run_id,
        actor="reviewer-1",
        payload={"review_hash": packet.review_hash, "reviewer": "reviewer-1"},
    )
    store.enqueue_control_action(action)

    result = HumanActionProcessor(store).process_pending()[0]
    approved = store.get_latest_review_packet(run_id)

    assert result.status.value == "done"
    assert approved is not None and approved.state is ReviewState.APPROVED
    assert approved.reviewer == "reviewer-1"
    assert ReviewCoordinator(store).can_deliver(run_id)
    assert store.get_task(run_id, "implement").state is TaskState.DONE
    store.close()


def test_rejected_review_returns_tasks_to_remediation_and_closes_gate(
    tmp_path: Path,
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()
    packet = ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="The implementation needs changes.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.REJECT_REVIEW,
        target_id="run",
        run_id=run_id,
        actor="reviewer-1",
        payload={
            "review_hash": packet.review_hash,
            "reviewer": "reviewer-1",
            "rationale": "Handle the empty input case.",
        },
    )
    store.enqueue_control_action(action)

    result = HumanActionProcessor(store).process_pending()[0]
    rejected = store.get_latest_review_packet(run_id)
    task = store.get_task(run_id, "implement")

    assert result.status.value == "done"
    assert rejected is not None and rejected.state is ReviewState.REJECTED
    assert task is not None and task.state is TaskState.READY
    assert "empty input" in (task.reason or "")
    assert not ReviewCoordinator(store).can_deliver(run_id)
    store.close()


def test_review_cannot_be_approved_after_integrated_diff_changes(
    tmp_path: Path,
) -> None:
    store, run_id, run_worktree = create_review_run(tmp_path)
    checks, scenarios = evidence()
    packet = ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="Ready for review.",
        verification_checks=checks,
        user_scenarios=scenarios,
    )
    (run_worktree / "feature.py").write_text("def feature():\n    return False\n")
    git(run_worktree, "add", "feature.py")
    commit(run_worktree, "change after review packet")
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.APPROVE_REVIEW,
        target_id="run",
        run_id=run_id,
        payload={"review_hash": packet.review_hash, "reviewer": "reviewer-1"},
    )
    store.enqueue_control_action(action)

    result = HumanActionProcessor(store).process_pending()[0]

    assert result.status.value == "failed"
    assert "diff changed" in (result.error or "")
    assert not ReviewCoordinator(store).can_deliver(run_id)
    assert store.get_task(run_id, "implement").state is TaskState.REVIEW
    store.close()


def test_failed_user_verification_cannot_create_an_approvable_review(
    tmp_path: Path,
) -> None:
    store, run_id, _ = create_review_run(tmp_path)
    checks, scenarios = evidence()
    failed_scenario = (
        UserScenarioCheck(
            scenario=scenarios[0].scenario,
            passed=False,
            evidence="The workflow did not complete.",
        ),
    )

    packet = ReviewCoordinator(store).prepare_review(
        run_id,
        review_summary="Verification found a problem.",
        verification_checks=checks,
        user_scenarios=failed_scenario,
    )

    assert packet.state is ReviewState.BLOCKED
    assert packet.blocking_findings
    assert store.get_latest_review_packet(run_id) == packet
    assert store.get_task(run_id, "implement").state is TaskState.READY
    assert not ReviewCoordinator(store).can_deliver(run_id)
    store.close()
