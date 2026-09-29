import asyncio
import hashlib
from datetime import UTC, datetime, timedelta
from pathlib import Path

from textual.widgets import DataTable, Static

from cronos_ai.dashboard import DashboardReadModel
from cronos_ai.dashboard_ui import FactoryDashboardApp
from cronos_ai.models import (
    ActivityEvent,
    Attempt,
    AttemptStatus,
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
    *,
    task_count: int = 1,
) -> Path:
    task_ids = tuple(f"task-{index}" for index in range(1, task_count + 1))
    plan_tasks = tuple(
        PlanTask(task_id=task_id, description=f"Work on {task_id}")
        for task_id in task_ids
    )
    records = tuple(
        TaskRecord(
            task_id=task_id,
            state=TaskState.RUNNING if index == 1 else TaskState.QUEUED,
            worker_id="worker-1" if index == 1 else None,
        )
        for index, task_id in enumerate(task_ids, start=1)
    )
    worktree = tmp_path / f"worktree-{run_id}"
    worktree.mkdir()
    plan_path = worktree / "openspec" / "changes" / f"change-{run_id}"
    plan_path.mkdir(parents=True)
    store.create_run(
        run_id,
        WorkRequest(
            request_id=f"request-{run_id}",
            description=f"Build feature for {run_id}",
            repo_path=tmp_path,
        ),
        OpenSpecPlan(
            change_name=f"change-{run_id}",
            tasks=plan_tasks,
        ),
        records,
    )
    store.save_run_context(
        RunContext(
            run_id=run_id,
            branch_name=f"factory/run/{run_id}",
            run_worktree_path=worktree,
        )
    )
    return worktree


def _add_review_packet(store: FactoryStore, run_id: str) -> None:
    diff_text = "diff --git a/app.py b/app.py"
    store.create_review_packet(
        ReviewPacket(
            review_id=f"review-{run_id}",
            run_id=run_id,
            base_commit="a" * 40,
            head_commit="b" * 40,
            diff_text=diff_text,
            diff_hash=hashlib.sha256(diff_text.encode()).hexdigest(),
            review_hash="c" * 64,
            review_summary="Implementation and verification are ready.",
            verification_checks=(
                VerificationCheck(
                    name="tests",
                    command="pytest",
                    passed=True,
                    summary="All tests passed.",
                ),
            ),
            user_scenarios=(
                UserScenarioCheck(
                    scenario="Operator inspects the result",
                    passed=True,
                    evidence="The dashboard displays the review.",
                ),
            ),
            state=ReviewState.READY,
        )
    )


def _add_event(
    store: FactoryStore,
    run_id: str,
    event_id: str,
    summary: str,
    seconds: int,
    task_id: str = "task-1",
) -> None:
    store.append_activity_event(
        ActivityEvent(
            event_id=event_id,
            run_id=run_id,
            task_id=task_id,
            occurred_at=datetime.now(UTC) + timedelta(seconds=seconds),
            category="progress",
            summary=summary,
        )
    )


def _static_text(app: FactoryDashboardApp, selector: str) -> str:
    return str(app.query_one(selector, Static).content)


def test_empty_dashboard_shows_controller_health_and_submit_guidance(
    tmp_path: Path,
) -> None:
    store = FactoryStore(tmp_path / "factory.sqlite3")
    app = FactoryDashboardApp(DashboardReadModel(store), refresh_interval=60)

    async def exercise() -> None:
        async with app.run_test(size=(80, 24)) as pilot:
            await pilot.pause(0.1)
            assert app.query_one("#runs-table", DataTable).row_count == 0
            assert "No factory runs" in _static_text(app, "#empty-state")
            assert "not started" in _static_text(app, "#controller-status")
            assert "cronos-ai run" in _static_text(app, "#empty-state")

    asyncio.run(exercise())
    store.close()


def test_operator_selects_runs_and_tasks_and_inspects_outputs_and_review(
    tmp_path: Path,
) -> None:
    store = FactoryStore(tmp_path / "factory.sqlite3")
    first_worktree = _create_run(store, tmp_path, "run-one", task_count=2)
    _create_run(store, tmp_path, "run-two")
    _add_review_packet(store, "run-one")
    _add_event(store, "run-one", "event-1", "Older activity", 1)
    _add_event(store, "run-one", "event-2", "Middle activity", 2)
    _add_event(store, "run-one", "event-3", "Newest activity", 3)
    store.record_attempt(
        "run-one",
        TaskRecord(
            task_id="task-1",
            state=TaskState.RUNNING,
            attempt_count=1,
            worker_id="worker-1",
        ),
        Attempt(
            task_id="task-1",
            attempt_number=1,
            status=AttemptStatus.RUNNING,
            worker_id="worker-1",
        ),
    )
    app = FactoryDashboardApp(
        DashboardReadModel(store, events_per_run=2),
        refresh_interval=60,
    )

    async def exercise() -> None:
        async with app.run_test(size=(120, 40)) as pilot:
            await pilot.pause(0.1)
            runs = app.query_one("#runs-table", DataTable)
            assert runs.row_count == 2
            assert "run-one" in _static_text(app, "#selected-run")
            assert "Implementation and verification are ready." in _static_text(
                app, "#review-summary"
            )
            output_text = _static_text(app, "#outputs")
            assert str(first_worktree) in output_text
            assert "OpenSpec change: available" in output_text
            assert "Newest activity" in _static_text(app, "#activity-log")
            assert "Middle activity" in _static_text(app, "#activity-log")
            assert "Older activity" not in _static_text(app, "#activity-log")
            assert "1/3" in _static_text(app, "#activity-page")

            runs.focus()
            await pilot.press("down", "enter")
            await pilot.pause(0.05)
            assert "run-two" in _static_text(app, "#selected-run")

            await pilot.press("up", "enter")
            await pilot.pause(0.05)
            tasks = app.query_one("#tasks-table", DataTable)
            tasks.focus()
            await pilot.press("down", "enter")
            await pilot.pause(0.05)
            assert "task-2" in _static_text(app, "#task-details")

            await pilot.press("n")
            await pilot.pause(0.05)
            assert "Older activity" in _static_text(app, "#activity-log")
            assert "2/3" in _static_text(app, "#activity-page")

    asyncio.run(exercise())
    store.close()


def test_dashboard_keeps_safe_activity_summaries_and_survives_narrow_terminal(
    tmp_path: Path,
    monkeypatch,
) -> None:
    store = FactoryStore(tmp_path / "factory.sqlite3")
    _create_run(store, tmp_path, "run-safe")
    secret = "provider-dashboard-secret-123456"
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", secret)
    _add_event(
        store,
        "run-safe",
        "event-secret",
        f"Worker reports {secret}",
        1,
    )
    app = FactoryDashboardApp(DashboardReadModel(store), refresh_interval=60)

    async def exercise() -> None:
        async with app.run_test(size=(40, 15)) as pilot:
            await pilot.pause(0.1)
            assert app.is_running
            rendered = _static_text(app, "#activity-log")
            assert "[REDACTED]" in rendered
            assert secret not in rendered
            await pilot.press("q")
            await pilot.pause(0.05)
            assert not app.is_running

    asyncio.run(exercise())
    store.close()


def test_end_to_end_dashboard_shows_blocked_work_progress_and_read_only_state(
    tmp_path: Path,
) -> None:
    store = FactoryStore(tmp_path / "factory.sqlite3")
    worktree = tmp_path / "worktree-run-e2e"
    worktree.mkdir()
    open_spec = worktree / "openspec" / "changes" / "change-run-e2e"
    open_spec.mkdir(parents=True)
    store.create_run(
        "run-e2e",
        WorkRequest(
            request_id="request-e2e",
            description="Resolve the integration conflict",
            repo_path=tmp_path,
        ),
        OpenSpecPlan(
            change_name="change-run-e2e",
            tasks=(
                PlanTask(
                    task_id="plan",
                    description="Plan implementation",
                    kind=PlanTaskKind.PLANNING,
                ),
                PlanTask(
                    task_id="merge",
                    description="Integrate changes",
                    kind=PlanTaskKind.IMPLEMENTATION,
                ),
                PlanTask(
                    task_id="verify",
                    description="Run checks",
                    kind=PlanTaskKind.VERIFICATION,
                ),
                PlanTask(
                    task_id="docs",
                    description="Review documentation",
                    kind=PlanTaskKind.VERIFICATION,
                ),
            ),
        ),
        (
            TaskRecord(task_id="plan", state=TaskState.DONE),
            TaskRecord(
                task_id="merge",
                state=TaskState.BLOCKED,
                reason="Merge conflict requires human resolution: src/app.py",
            ),
            TaskRecord(task_id="verify", state=TaskState.QUEUED),
            TaskRecord(task_id="docs", state=TaskState.READY),
        ),
    )
    store.save_run_context(
        RunContext(
            run_id="run-e2e",
            branch_name="factory/run/e2e",
            run_worktree_path=worktree,
        )
    )
    _add_review_packet(store, "run-e2e")
    _add_event(
        store,
        "run-e2e",
        "event-e2e",
        "Blocked awaiting human resolution",
        10,
        task_id="merge",
    )
    before = (
        store.get_run("run-e2e"),
        store.list_tasks("run-e2e"),
        store.get_attempts("run-e2e", "merge"),
        store.list_control_action_records(),
        store.list_activity_events("run-e2e"),
    )
    app = FactoryDashboardApp(DashboardReadModel(store), refresh_interval=60)

    async def exercise() -> None:
        async with app.run_test(size=(120, 40)) as pilot:
            await pilot.pause(0.1)
            summary = _static_text(app, "#selected-run")
            assert "awaiting human" in summary
            assert "implementation" in summary
            assert "1/4 tasks (25% completed)" in summary
            assert "done 1" in summary
            assert "blocked 1" in summary
            assert "queued 1" in summary
            assert "ready 1" in summary
            assert "Merge conflict requires human resolution" in _static_text(
                app, "#attention-list"
            )
            assert "resolve-conflict" in _static_text(app, "#attention-list")
            assert "Blocked awaiting human resolution" in _static_text(
                app, "#activity-log"
            )
            assert "OpenSpec change: available" in _static_text(app, "#outputs")
            assert "Implementation and verification are ready." in _static_text(
                app, "#review-summary"
            )

    asyncio.run(exercise())
    after = (
        store.get_run("run-e2e"),
        store.list_tasks("run-e2e"),
        store.get_attempts("run-e2e", "merge"),
        store.list_control_action_records(),
        store.list_activity_events("run-e2e"),
    )
    store.close()
    assert before == after
