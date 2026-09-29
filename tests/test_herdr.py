import json
import socket
import threading
from pathlib import Path
from typing import Any
from uuid import uuid4

import pytest

from cronos_ai.herdr import (
    HerdrAdapter,
    HerdrAPIError,
    HerdrError,
    HerdrSocketClient,
    HerdrTransportError,
    herdr_socket_path,
)
from cronos_ai.models import TaskState, WorkerStatus
from cronos_ai.storage import FactoryStore


class FakeProcess:
    def poll(self) -> int | None:
        return None


class FakeHerdrSocket:
    def __init__(self) -> None:
        self.running = False
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.workspaces: dict[str, dict[str, Any]] = {}
        self.panes: dict[str, dict[str, Any]] = {}

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((method, params))
        if method == "ping":
            if not self.running:
                raise HerdrTransportError("server unavailable")
            return {"type": "pong"}
        if not self.running:
            raise HerdrTransportError("server unavailable")
        if method == "workspace.list":
            return {"workspaces": list(self.workspaces.values())}
        if method == "workspace.create":
            workspace_id = f"w{len(self.workspaces) + 1}"
            pane_id = f"{workspace_id}:p1"
            workspace = {
                "workspace_id": workspace_id,
                "label": params.get("label", "Factory"),
            }
            pane = {
                "workspace_id": workspace_id,
                "pane_id": pane_id,
                "agent_status": "idle",
            }
            self.workspaces[workspace_id] = workspace
            self.panes[pane_id] = pane
            return {
                "workspace": workspace,
                "root_pane": pane,
            }
        if method == "pane.split":
            workspace_id = params["workspace_id"]
            pane_id = f"{workspace_id}:p{len(self.panes) + 1}"
            pane = {
                "workspace_id": workspace_id,
                "pane_id": pane_id,
                "agent_status": "idle",
            }
            self.panes[pane_id] = pane
            return {"pane": pane}
        if method == "pane.get":
            pane = self.panes.get(params["pane_id"])
            if pane is None:
                raise HerdrAPIError("pane_not_found", "pane not found")
            return {"pane": pane}
        if method == "pane.report_agent":
            pane = self.panes[params["pane_id"]]
            pane["agent"] = params["agent"]
            pane["agent_status"] = params["state"]
            return {"accepted": True}
        if method == "pane.report_metadata":
            return {"accepted": True}
        raise AssertionError(f"unexpected Herdr Socket API method: {method}")


class FakeHerdrCLI:
    def __init__(self, api: FakeHerdrSocket) -> None:
        self.api = api
        self.calls: list[tuple[list[str], dict[str, Any]]] = []

    def start_server(self, command: list[str], **kwargs: Any) -> FakeProcess:
        self.calls.append((command, kwargs))
        self.api.running = True
        return FakeProcess()


def make_adapter(
    database_path: Path,
    api: FakeHerdrSocket,
    cli: FakeHerdrCLI,
) -> HerdrAdapter:
    store = FactoryStore(database_path)
    return HerdrAdapter(
        "software-factory",
        store,
        socket_client=api,
        process_starter=cli.start_server,
        startup_timeout=0.1,
    )


@pytest.mark.parametrize(
    ("task_state", "worker_state", "herdr_state"),
    [
        (TaskState.QUEUED, WorkerStatus.IDLE, "idle"),
        (TaskState.READY, WorkerStatus.IDLE, "idle"),
        (TaskState.RUNNING, WorkerStatus.WORKING, "working"),
        (TaskState.INTEGRATING, WorkerStatus.WORKING, "working"),
        (TaskState.WAITING_FOR_HUMAN, WorkerStatus.BLOCKED, "blocked"),
        (TaskState.BLOCKED, WorkerStatus.BLOCKED, "blocked"),
        (TaskState.REVIEW, WorkerStatus.BLOCKED, "blocked"),
        (TaskState.FAILED, WorkerStatus.BLOCKED, "blocked"),
        (TaskState.DONE, WorkerStatus.DONE, "idle"),
    ],
)
def test_task_state_reports_truthful_coarse_sidebar_state(
    tmp_path: Path,
    task_state: TaskState,
    worker_state: WorkerStatus,
    herdr_state: str,
) -> None:
    api = FakeHerdrSocket()
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    api.running = True
    slot = adapter.ensure_slot("worker-1", tmp_path)

    adapter.report_task_state(
        "worker-1",
        "task-1",
        task_state,
        "Implement and verify the requested behavior.",
        reason="Awaiting human review" if task_state is TaskState.REVIEW else None,
    )

    updated = adapter.store.get_worker_slot("worker-1")
    report = next(
        params
        for method, params in reversed(api.calls)
        if method == "pane.report_agent"
    )
    metadata = next(
        params
        for method, params in reversed(api.calls)
        if method == "pane.report_metadata"
    )
    assert updated is not None
    assert updated.status is worker_state
    assert updated.active_task_id == (
        None if worker_state in (WorkerStatus.IDLE, WorkerStatus.DONE) else "task-1"
    )
    assert api.panes[slot.pane_id]["agent_status"] == herdr_state
    if task_state is TaskState.DONE:
        assert metadata["state_labels"] == {"idle": "Done"}
    else:
        assert metadata["clear_state_labels"] is True
    assert report["message"].startswith("task-1: Implement and verify")
    if task_state is TaskState.REVIEW:
        assert "Awaiting human review" in report["message"]
    adapter.store.close()


def test_task_sidebar_summary_is_bounded(tmp_path: Path) -> None:
    api = FakeHerdrSocket()
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    api.running = True
    adapter.ensure_slot("worker-1", tmp_path)

    adapter.report_task_state(
        "worker-1",
        "task-long",
        TaskState.RUNNING,
        "x" * 500,
    )

    report = next(
        params
        for method, params in reversed(api.calls)
        if method == "pane.report_agent"
    )
    assert len(report["message"]) <= 240
    assert report["message"].endswith("...")
    adapter.store.close()


def test_named_socket_path_uses_directory_of_herdr_config_file(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    config_file = tmp_path / "custom-herdr.toml"
    monkeypatch.setenv("HERDR_CONFIG_PATH", str(config_file))
    monkeypatch.setenv("HERDR_SOCKET_PATH", str(tmp_path / "default.sock"))

    assert herdr_socket_path("software-factory") == (
        tmp_path / "sessions" / "software-factory" / "herdr.sock"
    )
    assert herdr_socket_path("default") == tmp_path / "default.sock"


def test_socket_client_uses_newline_delimited_json_rpc() -> None:
    socket_path = Path("/tmp") / f"herdr-{uuid4().hex[:8]}.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(socket_path))
    server.listen(1)
    received: list[dict[str, Any]] = []

    def respond() -> None:
        connection, _ = server.accept()
        with connection:
            request_line = connection.recv(4096).split(b"\n", 1)[0]
            request = json.loads(request_line)
            received.append(request)
            connection.sendall(
                json.dumps(
                    {
                        "id": request["id"],
                        "result": {"type": "pong"},
                    }
                ).encode()
                + b"\n"
            )

    thread = threading.Thread(target=respond, daemon=True)
    thread.start()
    try:
        result = HerdrSocketClient(socket_path).call("ping", {})
    finally:
        thread.join(timeout=2)
        server.close()
        socket_path.unlink(missing_ok=True)

    assert result == {"type": "pong"}
    assert received[0]["method"] == "ping"
    assert received[0]["params"] == {}


def test_ensure_slot_starts_named_session_and_creates_first_pane(
    tmp_path: Path,
) -> None:
    api = FakeHerdrSocket()
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    worker_cwd = tmp_path / "worktree"
    worker_cwd.mkdir()

    slot = adapter.ensure_slot("worker-1", worker_cwd)

    assert slot.session_id == "software-factory"
    assert slot.workspace_id == "w1"
    assert slot.pane_id == "w1:p1"
    assert slot.status is WorkerStatus.IDLE
    assert cli.calls[0][0] == ["herdr", "--session", "software-factory", "server"]
    workspace_call = next(
        params for method, params in api.calls if method == "workspace.create"
    )
    assert workspace_call["label"] == "Cronos AI (software-factory)"
    assert any(method == "pane.report_agent" for method, _ in api.calls)
    adapter.store.close()


def test_additional_worker_slots_use_distinct_panes_in_the_workspace(
    tmp_path: Path,
) -> None:
    api = FakeHerdrSocket()
    api.running = True
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    first_cwd = tmp_path / "first-worktree"
    second_cwd = tmp_path / "second-worktree"
    first_cwd.mkdir()
    second_cwd.mkdir()

    first = adapter.ensure_slot("worker-1", first_cwd)
    second = adapter.ensure_slot("worker-2", second_cwd)

    assert first.workspace_id == second.workspace_id
    assert first.pane_id != second.pane_id
    assert sum(method == "workspace.create" for method, _ in api.calls) == 1
    assert sum(method == "pane.split" for method, _ in api.calls) == 1
    adapter.store.close()


def test_reconciliation_reuses_live_pane_after_adapter_restart(
    tmp_path: Path,
) -> None:
    database_path = tmp_path / "factory.sqlite3"
    api = FakeHerdrSocket()
    cli = FakeHerdrCLI(api)
    worker_cwd = tmp_path / "worktree"
    worker_cwd.mkdir()

    first_adapter = make_adapter(database_path, api, cli)
    original = first_adapter.ensure_slot("worker-1", worker_cwd)
    first_adapter.store.close()
    api.calls.clear()
    cli.calls.clear()

    restarted = make_adapter(database_path, api, cli)
    reconciled = restarted.reconcile_workers()

    assert len(reconciled) == 1
    assert reconciled[0].live is True
    assert reconciled[0].slot == original
    assert sum(method == "pane.get" for method, _ in api.calls) == 1
    assert not any(
        method in {"workspace.create", "pane.split"} for method, _ in api.calls
    )
    assert cli.calls == []
    restarted.store.close()


def test_missing_active_pane_is_reported_without_creating_a_duplicate(
    tmp_path: Path,
) -> None:
    database_path = tmp_path / "factory.sqlite3"
    api = FakeHerdrSocket()
    api.running = True
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(database_path, api, cli)
    worker_cwd = tmp_path / "worktree"
    worker_cwd.mkdir()
    slot = adapter.ensure_slot("worker-1", worker_cwd)
    adapter.store.save_worker_slot(
        slot.model_copy(
            update={
                "status": WorkerStatus.WORKING,
                "active_task_id": "task-1",
            }
        )
    )
    del api.panes[slot.pane_id]
    api.calls.clear()

    reconciled = adapter.reconcile_workers()

    assert len(reconciled) == 1
    assert reconciled[0].live is False
    assert reconciled[0].slot.active_task_id == "task-1"
    assert not any(
        method in {"workspace.create", "pane.split"} for method, _ in api.calls
    )
    assert adapter.store.get_worker_slot("worker-1").pane_id == slot.pane_id
    adapter.store.close()


def test_a_task_cannot_be_assigned_to_two_worker_slots(tmp_path: Path) -> None:
    api = FakeHerdrSocket()
    api.running = True
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    first_cwd = tmp_path / "first-worktree"
    second_cwd = tmp_path / "second-worktree"
    first_cwd.mkdir()
    second_cwd.mkdir()
    first = adapter.ensure_slot("worker-1", first_cwd)
    second = adapter.ensure_slot("worker-2", second_cwd)
    adapter.assign_task(first.worker_id, "task-1", "Implement feature")

    with pytest.raises(HerdrError, match="already assigned"):
        adapter.assign_task(second.worker_id, "task-1", "Implement feature")

    assert adapter.store.get_worker_slot(second.worker_id).status is WorkerStatus.IDLE
    adapter.store.close()


def test_completed_worker_slot_is_reusable_without_stale_done_label(
    tmp_path: Path,
) -> None:
    api = FakeHerdrSocket()
    api.running = True
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    worker_cwd = tmp_path / "worktree"
    worker_cwd.mkdir()
    slot = adapter.ensure_slot("worker-1", worker_cwd)

    adapter.assign_task(slot.worker_id, "task-1", "First task")
    done = adapter.report_worker_status(
        slot.worker_id,
        WorkerStatus.DONE,
        "First task completed",
    )
    done_metadata = [
        params for method, params in api.calls if method == "pane.report_metadata"
    ][-1]
    reused = adapter.assign_task(slot.worker_id, "task-2", "Next task")
    working_metadata = [
        params for method, params in api.calls if method == "pane.report_metadata"
    ][-1]

    assert done.status is WorkerStatus.DONE
    assert done.active_task_id is None
    assert done_metadata["state_labels"] == {"idle": "Done"}
    assert reused.status is WorkerStatus.WORKING
    assert reused.active_task_id == "task-2"
    assert working_metadata["clear_state_labels"] is True
    adapter.store.close()


def test_task_assignment_reports_structured_worker_status(
    tmp_path: Path,
) -> None:
    api = FakeHerdrSocket()
    api.running = True
    cli = FakeHerdrCLI(api)
    adapter = make_adapter(tmp_path / "factory.sqlite3", api, cli)
    worker_cwd = tmp_path / "worktree"
    worker_cwd.mkdir()
    slot = adapter.ensure_slot("worker-1", worker_cwd)

    assigned = adapter.assign_task(slot.worker_id, "task-1", "Implement the feature")
    report = next(
        params
        for method, params in api.calls
        if method == "pane.report_agent" and params.get("state") == "working"
    )

    assert assigned.active_task_id == "task-1"
    assert assigned.status is WorkerStatus.WORKING
    assert report["pane_id"] == slot.pane_id
    assert report["message"] == "task-1: Implement the feature"
    adapter.store.close()
