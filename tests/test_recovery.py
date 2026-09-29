from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from cronos_ai.herdr import HerdrAdapter, HerdrAPIError
from cronos_ai.models import (
    Attempt,
    AttemptStatus,
    OpenSpecPlan,
    PlanTask,
    TaskRecord,
    TaskState,
    WorkerSlot,
    WorkerStatus,
    WorkRequest,
)
from cronos_ai.recovery import (
    RecoveryDisposition,
    TaskRecoveryManager,
)
from cronos_ai.storage import FactoryStore


class FakeHerdrSocket:
    def __init__(self, panes: dict[str, dict[str, Any]]) -> None:
        self.panes = panes
        self.calls: list[str] = []

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.calls.append(method)
        if method == "ping":
            return {"type": "pong"}
        if method == "pane.get":
            pane = self.panes.get(params["pane_id"])
            if pane is None:
                raise HerdrAPIError("pane_not_found", "pane not found")
            return {"pane": pane}
        raise AssertionError(f"unexpected Herdr method: {method}")


def setup_running_task(
    database_path: Path,
    *,
    attempt_number: int = 1,
    slot_task_id: str = "task-1",
) -> tuple[FactoryStore, HerdrAdapter]:
    request = WorkRequest(
        request_id="request-1",
        description="Continue the interrupted task",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="continue-task",
        tasks=(PlanTask(task_id="task-1", description="Continue work"),),
    )
    store = FactoryStore(database_path)
    store.create_run(
        "run-1",
        request,
        plan,
        (TaskRecord(task_id="task-1", state=TaskState.QUEUED),),
    )
    if attempt_number > 1:
        previous_attempt = Attempt(
            task_id="task-1",
            attempt_number=1,
            status=AttemptStatus.FAILED,
            started_at=datetime.now(UTC),
            finished_at=datetime.now(UTC),
            reason="temporary error",
            transient=True,
        )
        store.record_attempt(
            "run-1",
            TaskRecord(
                task_id="task-1",
                state=TaskState.READY,
                attempt_count=1,
            ),
            previous_attempt,
        )
    running_attempt = Attempt(
        task_id="task-1",
        attempt_number=attempt_number,
        status=AttemptStatus.RUNNING,
        started_at=datetime.now(UTC),
        worker_id="worker-1",
    )
    store.record_attempt(
        "run-1",
        TaskRecord(
            task_id="task-1",
            state=TaskState.RUNNING,
            attempt_count=attempt_number,
            worker_id="worker-1",
        ),
        running_attempt,
    )
    slot = WorkerSlot(
        worker_id="worker-1",
        session_id="software-factory",
        workspace_id="w1",
        pane_id="w1:p1",
        status=WorkerStatus.WORKING,
        active_task_id=slot_task_id,
    )
    store.save_worker_slot(slot)
    return store, HerdrAdapter(
        "software-factory",
        store,
        socket_client=FakeHerdrSocket({
            "w1:p1": {
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "agent": "factory-worker-1",
                "agent_status": "working",
            }
        }),
    )


def test_recovery_reconnects_a_surviving_worker_without_redispatch(
    tmp_path: Path,
) -> None:
    store, herdr = setup_running_task(tmp_path / "factory.sqlite3")

    outcomes = TaskRecoveryManager(store, max_attempts=3).recover("run-1", herdr)

    assert len(outcomes) == 1
    assert outcomes[0].disposition is RecoveryDisposition.RECONNECTED
    assert store.get_task("run-1", "task-1").state is TaskState.RUNNING
    assert store.get_attempts("run-1", "task-1")[0].status is AttemptStatus.RUNNING
    assert outcomes[0].slot.worker_id == "worker-1"
    store.close()


def test_missing_worker_records_interrupted_attempt_and_retries_boundedly(
    tmp_path: Path,
) -> None:
    store, herdr = setup_running_task(tmp_path / "factory.sqlite3")
    herdr._socket.panes.clear()

    outcomes = TaskRecoveryManager(store, max_attempts=3).recover("run-1", herdr)

    task = store.get_task("run-1", "task-1")
    attempts = store.get_attempts("run-1", "task-1")
    slot = store.get_worker_slot("worker-1")
    assert outcomes[0].disposition is RecoveryDisposition.RETRY
    assert task.state is TaskState.READY
    assert task.worker_id is None
    assert attempts[0].status is AttemptStatus.INTERRUPTED
    assert "restart" in attempts[0].reason.lower()
    assert slot.status is WorkerStatus.IDLE
    assert slot.active_task_id is None
    assert not store.active_worker_ids()
    assert not any(
        method in {"workspace.create", "pane.split"}
        for method in herdr._socket.calls
    )
    store.close()


def test_restart_does_not_exceed_maximum_attempt_count(tmp_path: Path) -> None:
    store, herdr = setup_running_task(
        tmp_path / "factory.sqlite3",
        attempt_number=2,
    )
    herdr._socket.panes.clear()

    outcomes = TaskRecoveryManager(store, max_attempts=2).recover("run-1", herdr)

    task = store.get_task("run-1", "task-1")
    attempts = store.get_attempts("run-1", "task-1")
    assert outcomes[0].disposition is RecoveryDisposition.FAILED
    assert task.state is TaskState.FAILED
    assert task.reason
    assert len(attempts) == 2
    assert attempts[1].status is AttemptStatus.INTERRUPTED
    store.close()


def test_recovery_blocks_mismatched_live_worker_association(tmp_path: Path) -> None:
    store, herdr = setup_running_task(
        tmp_path / "factory.sqlite3",
        slot_task_id="another-task",
    )

    outcomes = TaskRecoveryManager(store, max_attempts=3).recover("run-1", herdr)

    task = store.get_task("run-1", "task-1")
    assert outcomes[0].disposition is RecoveryDisposition.BLOCKED
    assert task.state is TaskState.BLOCKED
    assert "does not match" in task.reason
    assert store.get_attempts("run-1", "task-1")[0].status is AttemptStatus.INTERRUPTED
    store.close()
