import sqlite3
from datetime import UTC, datetime, timedelta

import pytest

from cronos_ai.models import (
    ApprovalDecision,
    Attempt,
    AttemptStatus,
    OpenSpecPlan,
    PlanApproval,
    PlanTask,
    TaskRecord,
    TaskState,
    WorkerSlot,
    WorkerStatus,
    WorkRequest,
)
from cronos_ai.storage import ControllerLockError, FactoryStore


def make_run() -> tuple[WorkRequest, OpenSpecPlan]:
    request = WorkRequest(
        request_id="request-1",
        description="Implement a small feature",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="small-feature",
        tasks=(PlanTask(task_id="task-1", description="Implement feature"),),
    )
    return request, plan


def make_attempt(
    *, number: int = 1, status: AttemptStatus = AttemptStatus.RUNNING
) -> Attempt:
    started_at = datetime.now(UTC)
    finished_at = (
        started_at + timedelta(seconds=1)
        if status is not AttemptStatus.RUNNING
        else None
    )
    reason = "worker failed" if status is AttemptStatus.FAILED else None
    return Attempt(
        task_id="task-1",
        attempt_number=number,
        status=status,
        started_at=started_at,
        finished_at=finished_at,
        reason=reason,
    )


def test_schema_migrates_and_task_attempt_survive_reopen(tmp_path) -> None:
    database_path = tmp_path / "factory.sqlite3"
    request, plan = make_run()
    task = TaskRecord(
        task_id="task-1",
        state=TaskState.RUNNING,
        attempt_count=1,
        worker_id="worker-1",
    )
    attempt = make_attempt()

    with FactoryStore(database_path) as store:
        store.create_run("run-1", request, plan)
        store.record_attempt("run-1", task, attempt)
        assert store.schema_version == 10

    with FactoryStore(database_path) as reopened:
        assert reopened.get_run("run-1") == (request, plan)
        assert reopened.get_task("run-1", "task-1") == task
        assert reopened.get_attempts("run-1", "task-1") == [attempt]


def test_task_and_attempt_are_written_atomically(tmp_path) -> None:
    request, plan = make_run()
    initial_task = TaskRecord(task_id="task-1", state=TaskState.DONE, attempt_count=1)
    updated_task = TaskRecord(
        task_id="task-1",
        state=TaskState.RUNNING,
        attempt_count=2,
        worker_id="worker-1",
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-1", request, plan)
        store.record_attempt(
            "run-1",
            initial_task,
            make_attempt(status=AttemptStatus.SUCCEEDED),
        )

        with pytest.raises(sqlite3.IntegrityError):
            store.record_attempt(
                "run-1",
                updated_task,
                make_attempt(status=AttemptStatus.SUCCEEDED),
            )

        assert store.get_task("run-1", "task-1") == initial_task
        assert len(store.get_attempts("run-1", "task-1")) == 1


def test_plan_approval_is_durable_and_scoped_to_its_plan(tmp_path) -> None:
    database_path = tmp_path / "factory.sqlite3"
    request, plan = make_run()
    approval = PlanApproval(
        change_name=plan.change_name,
        plan_hash="a" * 64,
        decision=ApprovalDecision.APPROVED,
        reviewer="reviewer-1",
    )
    rejection = PlanApproval(
        change_name=plan.change_name,
        plan_hash="a" * 64,
        decision=ApprovalDecision.REJECTED,
        reviewer="reviewer-2",
        rationale="Please revise the plan.",
    )

    with FactoryStore(database_path) as store:
        store.create_run("run-1", request, plan)
        store.record_plan_approval("run-1", approval)
        store.record_plan_approval("run-1", rejection)

    with FactoryStore(database_path) as reopened:
        assert (
            reopened.get_plan_approval("run-1", plan.change_name, "a" * 64) == rejection
        )
        assert reopened.get_plan_approval("run-1", plan.change_name, "b" * 64) is None


def test_plan_approval_must_match_the_run_plan(tmp_path) -> None:
    request, plan = make_run()
    approval = PlanApproval(
        change_name="different-change",
        plan_hash="a" * 64,
        decision=ApprovalDecision.APPROVED,
        reviewer="reviewer-1",
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-1", request, plan)
        with pytest.raises(ValueError, match="does not match the run plan"):
            store.record_plan_approval("run-1", approval)

        assert store.get_plan_approval("run-1", "different-change", "a" * 64) is None


def test_worker_slot_association_survives_database_reopen(tmp_path) -> None:
    database_path = tmp_path / "factory.sqlite3"
    slot = WorkerSlot(
        worker_id="worker-1",
        session_id="software-factory",
        workspace_id="w1",
        pane_id="w1:p1",
        status=WorkerStatus.IDLE,
    )

    with FactoryStore(database_path) as store:
        store.save_worker_slot(slot)

    with FactoryStore(database_path) as reopened:
        assert reopened.get_worker_slot("worker-1") == slot
        assert reopened.list_worker_slots("software-factory") == [slot]


def test_only_one_controller_can_hold_the_process_lock(tmp_path) -> None:
    database_path = tmp_path / "factory.sqlite3"
    first = FactoryStore(database_path)
    second = FactoryStore(database_path)

    try:
        first.acquire_controller_lock()
        with pytest.raises(ControllerLockError):
            second.acquire_controller_lock()

        first.close()
        second.acquire_controller_lock()
    finally:
        first.close()
        second.close()
