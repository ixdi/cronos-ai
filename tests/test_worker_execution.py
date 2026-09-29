from pathlib import Path
from typing import Any

import pytest

from cronos_ai.herdr import HerdrAdapter
from cronos_ai.models import SpecialistProfile, WorkerStatus
from cronos_ai.pi_rpc import PiProcessExited, PiProtocolError
from cronos_ai.sandbox import DockerSandboxAdapter
from cronos_ai.scheduler import TransientWorkerError
from cronos_ai.storage import FactoryStore
from cronos_ai.worker_execution import (
    PiWorkerExecutor,
    WorkerExecutionError,
)


class FakeHerdrSocket:
    def __init__(self) -> None:
        self.pane = {
            "workspace_id": "w1",
            "pane_id": "w1:p1",
            "agent": None,
            "agent_status": "idle",
        }

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if method == "ping":
            return {"type": "pong"}
        if method == "workspace.create":
            return {
                "workspace": {"workspace_id": "w1"},
                "root_pane": self.pane,
            }
        if method == "pane.get":
            return {"pane": self.pane}
        if method == "pane.report_agent":
            self.pane["agent"] = params["agent"]
            self.pane["agent_status"] = params["state"]
            return {"accepted": True}
        if method == "pane.report_metadata":
            return {"accepted": True}
        raise AssertionError(f"unexpected Herdr method: {method}")


def make_executor(
    tmp_path: Path,
    process_factory,
) -> tuple[PiWorkerExecutor, FactoryStore]:
    store = FactoryStore(tmp_path / "factory.sqlite3")
    herdr = HerdrAdapter(
        "software-factory",
        store,
        socket_client=FakeHerdrSocket(),
    )
    profile = SpecialistProfile(
        name="implementer",
        role="Implementation",
        model="model-a",
        provider="provider-a",
        provider_api_key_env="FACTORY_PROVIDER_KEY",
    )
    executor = PiWorkerExecutor(
        herdr,
        DockerSandboxAdapter("worker-image"),
        profile,
        command=("pi",),
        process_factory=process_factory,
    )
    return executor, store


def test_interrupted_pi_process_releases_slot_for_bounded_retry(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = tmp_path / "task-worktree"
    worktree.mkdir()

    def exited_process(*args: object, **kwargs: object) -> None:
        raise PiProcessExited("Pi exited before agent_settled")

    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "test-provider-key")
    executor, store = make_executor(tmp_path, exited_process)

    with pytest.raises(TransientWorkerError, match="transient Pi worker failure"):
        executor.execute(
            worktree,
            worker_id="worker-1",
            task_id="task-1",
            prompt="Implement the task",
        )

    slot = store.get_worker_slot("worker-1")
    assert slot.status is WorkerStatus.IDLE
    assert slot.active_task_id is None
    store.close()


def test_malformed_pi_protocol_blocks_worker_slot_for_attention(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = tmp_path / "task-worktree"
    worktree.mkdir()

    def malformed_protocol(*args: object, **kwargs: object) -> None:
        raise PiProtocolError("Pi emitted invalid JSONL")

    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "test-provider-key")
    executor, store = make_executor(tmp_path, malformed_protocol)

    with pytest.raises(WorkerExecutionError, match="worker failed"):
        executor.execute(
            worktree,
            worker_id="worker-1",
            task_id="task-1",
            prompt="Implement the task",
        )

    slot = store.get_worker_slot("worker-1")
    assert slot.status is WorkerStatus.BLOCKED
    assert slot.active_task_id == "task-1"
    store.close()
