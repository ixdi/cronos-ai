import hashlib
from datetime import UTC, datetime, timedelta
from pathlib import Path

from cronos_ai.dashboard import DashboardReadModel
from cronos_ai.models import (
    ActivityEvent,
    Attempt,
    AttemptStatus,
    ControlAction,
    ControlActionType,
    ControllerLifecycle,
    ControllerStatus,
    OpenSpecPlan,
    PlanTask,
    PlanTaskKind,
    ReviewPacket,
    ReviewState,
    RunContext,
    TaskRecord,
    TaskState,
    UserScenarioCheck,
    VerificationCheck,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore


def _create_run(
    store: FactoryStore,
    tmp_path: Path,
    run_id: str,
    plan_tasks: tuple[PlanTask, ...],
    records: tuple[TaskRecord, ...],
) -> None:
    store.create_run(
        run_id,
        WorkRequest(
            request_id=f"request-{run_id}",
            description=f"Request for {run_id}",
            repo_path=tmp_path,
        ),
        OpenSpecPlan(
            change_name=f"change-{run_id}",
            tasks=plan_tasks,
        ),
        records,
    )


def test_running_run_aggregates_stage_workers_and_rounded_task_progress(
    tmp_path: Path,
) -> None:
    plan_tasks = (
        PlanTask(task_id="task-1", description="Plan", kind=PlanTaskKind.PLANNING),
        PlanTask(
            task_id="task-2",
            description="Implement",
            kind=PlanTaskKind.IMPLEMENTATION,
        ),
        *(
            PlanTask(task_id=f"task-{number}", description=f"Queued {number}")
            for number in range(3, 9)
        ),
    )
    records = (
        TaskRecord(task_id="task-1", state=TaskState.DONE),
        TaskRecord(
            task_id="task-2",
            state=TaskState.RUNNING,
            worker_id="worker-1",
        ),
        *(
            TaskRecord(task_id=f"task-{number}", state=TaskState.QUEUED)
            for number in range(3, 9)
        ),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, "run-active", plan_tasks, records)

        snapshot = DashboardReadModel(store).snapshot()

    run = snapshot.runs[0]
    assert run.run_id == "run-active"
    assert run.state == "running"
    assert run.current_stage == "implementation"
    assert run.task_counts["running"] == 1
    assert run.task_counts["queued"] == 6
    assert run.completed_tasks == 1
    assert run.total_tasks == 8
    assert run.completion_percentage == 13
    assert run.worker_assignments == (("task-2", "worker-1"),)


def test_queued_run_shows_next_incomplete_stage_and_exact_progress(
    tmp_path: Path,
) -> None:
    plan_tasks = (
        PlanTask(task_id="plan", description="Plan", kind=PlanTaskKind.PLANNING),
        PlanTask(
            task_id="verify",
            description="Verify",
            kind=PlanTaskKind.VERIFICATION,
        ),
    )
    records = (
        TaskRecord(task_id="plan", state=TaskState.DONE),
        TaskRecord(task_id="verify", state=TaskState.QUEUED),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, "run-queued", plan_tasks, records)

        run = DashboardReadModel(store).snapshot().runs[0]

    assert run.state == "queued"
    assert run.current_stage == "verification"
    assert run.completion_percentage == 50


def test_run_with_human_attention_is_marked_awaiting_human(
    tmp_path: Path,
) -> None:
    plan_tasks = (
        PlanTask(
            task_id="merge",
            description="Integrate task changes",
            kind=PlanTaskKind.IMPLEMENTATION,
        ),
    )
    records = (
        TaskRecord(
            task_id="merge",
            state=TaskState.BLOCKED,
            reason="Merge conflict requires human resolution.",
        ),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, "run-attention", plan_tasks, records)

        run = DashboardReadModel(store).snapshot().runs[0]

    assert run.state == "awaiting human"
    assert len(run.attention_items) == 1
    assert run.attention_items[0].available_actions


def test_run_details_include_attempts_paths_review_activity_and_attention(
    tmp_path: Path,
    monkeypatch,
) -> None:
    run_worktree = tmp_path / "run-worktree"
    run_worktree.mkdir()
    provider_secret = "provider-review-secret-123456"
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", provider_secret)
    plan_tasks = (
        PlanTask(
            task_id="implement",
            description="Implement dashboard",
            kind=PlanTaskKind.IMPLEMENTATION,
        ),
    )
    initial_tasks = (TaskRecord(task_id="implement"),)
    started_at = datetime(2026, 9, 29, 12, 30, tzinfo=UTC)
    running_attempt = Attempt(
        task_id="implement",
        attempt_number=1,
        status=AttemptStatus.RUNNING,
        started_at=started_at,
        worker_id="worker-1",
    )
    completed_attempt = Attempt(
        task_id="implement",
        attempt_number=1,
        status=AttemptStatus.SUCCEEDED,
        started_at=started_at,
        finished_at=started_at + timedelta(minutes=1),
        worker_id="worker-1",
    )
    diff_text = "diff --git a/file b/file"
    review_packet = ReviewPacket(
        review_id="review-1",
        run_id="run-details",
        base_commit="a" * 40,
        head_commit="b" * 40,
        diff_text=diff_text,
        diff_hash=hashlib.sha256(diff_text.encode()).hexdigest(),
        review_hash="c" * 64,
        review_summary=f"Review passed with token {provider_secret}",
        verification_checks=(
            VerificationCheck(
                name="tests",
                command="pytest",
                passed=True,
                summary="Tests passed",
            ),
        ),
        user_scenarios=(
            UserScenarioCheck(
                scenario="Dashboard loads",
                passed=True,
                evidence="Observed in terminal",
            ),
        ),
        state=ReviewState.READY,
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, "run-details", plan_tasks, initial_tasks)
        store.save_run_context(
            RunContext(
                run_id="run-details",
                branch_name="factory/run/details",
                run_worktree_path=run_worktree,
            )
        )
        store.record_attempt(
            "run-details",
            TaskRecord(
                task_id="implement",
                state=TaskState.RUNNING,
                attempt_count=1,
                worker_id="worker-1",
            ),
            running_attempt,
        )
        store.finish_attempt(
            "run-details",
            TaskRecord(task_id="implement", state=TaskState.REVIEW, attempt_count=1),
            completed_attempt,
        )
        store.create_review_packet(review_packet)
        store.append_activity_event(
            ActivityEvent(
                event_id="progress-1",
                run_id="run-details",
                task_id="implement",
                occurred_at=started_at + timedelta(seconds=30),
                category="progress",
                summary="Running test suite",
            )
        )
        store.set_controller_status(
            ControllerStatus(
                state=ControllerLifecycle.RUNNING,
                pid=12345,
                started_at=started_at,
                heartbeat_at=started_at + timedelta(minutes=1),
            )
        )

        snapshot = DashboardReadModel(store).snapshot()

    run = snapshot.runs[0]
    assert snapshot.controller_status is not None
    assert snapshot.controller_status.state is ControllerLifecycle.RUNNING
    assert run.worktree_path == run_worktree
    assert run.open_spec_path == (
        run_worktree / "openspec" / "changes" / "change-run-details"
    )
    assert run.review_summary == "Review passed with token [REDACTED]"
    assert provider_secret not in run.review_summary
    assert run.tasks[0].attempts[0].status is AttemptStatus.SUCCEEDED
    assert run.tasks[0].attempts[0].worker_id == "worker-1"
    progress_events = [
        event.summary
        for event in run.activity_events
        if event.event_id == "progress-1"
    ]
    assert progress_events == ["Running test suite"]
    assert any(item.item_id == "run-details:review" for item in run.attention_items)


def test_repeated_dashboard_snapshots_do_not_mutate_factory_state(
    tmp_path: Path,
) -> None:
    run_id = "run-read-only"
    plan_hash = "a" * 64
    plan_tasks = (PlanTask(task_id="task-1", description="Inspect state"),)
    records = (
        TaskRecord(
            task_id="task-1",
            state=TaskState.FAILED,
            reason="A prior attempt failed.",
            attempt_count=1,
        ),
    )
    request = WorkRequest(
        request_id="queued-request",
        description="Leave this queued",
        repo_path=tmp_path,
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, run_id, plan_tasks, records)
        store.save_run_context(
            RunContext(
                run_id=run_id,
                branch_name="factory/run/read-only",
                run_worktree_path=tmp_path / "run-worktree",
                plan_hash=plan_hash,
                approval_required=True,
            )
        )
        store.enqueue_request(request)
        store.enqueue_control_action(
            ControlAction(
                action_id="pending-action",
                action_type=ControlActionType.CLARIFY_REQUEST,
                target_id=request.request_id,
                payload={"answer": "Keep this action pending"},
            )
        )
        before = (
            store.list_run_ids(),
            store.get_run(run_id),
            store.list_tasks(run_id),
            store.get_attempts(run_id, "task-1"),
            store.list_requests(),
            store.list_control_action_records(),
            store.get_plan_approval(run_id, f"change-{run_id}", plan_hash),
            store.list_activity_events(run_id),
        )

        model = DashboardReadModel(store)
        snapshots = (model.snapshot(), model.snapshot(), model.snapshot())

        after = (
            store.list_run_ids(),
            store.get_run(run_id),
            store.list_tasks(run_id),
            store.get_attempts(run_id, "task-1"),
            store.list_requests(),
            store.list_control_action_records(),
            store.get_plan_approval(run_id, f"change-{run_id}", plan_hash),
            store.list_activity_events(run_id),
        )

    assert len(snapshots) == 3
    assert before == after


def test_unmatched_active_task_has_unknown_current_stage(tmp_path: Path) -> None:
    plan_tasks = (
        PlanTask(task_id="task-1", description="Implement"),
        PlanTask(
            task_id="task-2",
            description="Verify",
            kind=PlanTaskKind.VERIFICATION,
        ),
    )
    records = (
        TaskRecord(task_id="task-1", state=TaskState.READY),
        TaskRecord(task_id="task-2", state=TaskState.QUEUED),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store, tmp_path, "run-unknown", plan_tasks, records)
        store.save_task(
            "run-unknown",
            TaskRecord(
                task_id="orphan-task",
                state=TaskState.RUNNING,
                worker_id="worker-orphan",
            ),
        )

        run = DashboardReadModel(store).snapshot().runs[0]

    assert run.current_stage == "unknown"
