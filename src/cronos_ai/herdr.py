"""Herdr session and worker-pane lifecycle adapter."""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol
from uuid import uuid4

from cronos_ai.models import TaskState, WorkerSlot, WorkerStatus
from cronos_ai.storage import FactoryStore


class HerdrError(RuntimeError):
    """Raised when Herdr cannot safely serve a factory worker slot."""


class HerdrTransportError(HerdrError):
    """Raised when the Herdr socket cannot be reached or decoded."""


class HerdrAPIError(HerdrError):
    """Raised for a structured Herdr Socket API error response."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        super().__init__(f"Herdr API error {code}: {message}")


class HerdrSocket(Protocol):
    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]: ...


@dataclass(frozen=True)
class ReconciledWorker:
    """A persisted slot and whether its Herdr pane survived controller restart."""

    slot: WorkerSlot
    live: bool


class HerdrSocketClient:
    """One-request-per-connection NDJSON client for the local Herdr socket."""

    def __init__(self, socket_path: Path, *, timeout: float = 2.0) -> None:
        self.socket_path = socket_path
        self.timeout = timeout

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if not hasattr(socket, "AF_UNIX"):
            raise HerdrTransportError("Herdr Unix socket transport is unavailable")
        request_id = f"factory-{uuid4().hex}"
        request = (
            json.dumps(
                {"id": request_id, "method": method, "params": params},
                separators=(",", ":"),
            ).encode("utf-8")
            + b"\n"
        )
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(self.timeout)
                client.connect(str(self.socket_path))
                client.sendall(request)
                response_data = bytearray()
                while b"\n" not in response_data:
                    chunk = client.recv(65536)
                    if not chunk:
                        raise HerdrTransportError(
                            "Herdr socket closed before returning a response"
                        )
                    response_data.extend(chunk)
                    if len(response_data) > 4 * 1024 * 1024:
                        raise HerdrTransportError("Herdr response exceeded size limit")
        except HerdrError:
            raise
        except OSError as error:
            raise HerdrTransportError("could not reach Herdr Socket API") from error

        try:
            response = json.loads(bytes(response_data).split(b"\n", 1)[0])
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise HerdrTransportError("Herdr returned invalid JSON") from error
        if not isinstance(response, dict) or response.get("id") != request_id:
            raise HerdrTransportError("Herdr response identifier did not match request")
        response_error = response.get("error")
        if isinstance(response_error, dict):
            raise HerdrAPIError(
                str(response_error.get("code", "unknown")),
                str(response_error.get("message", "request failed")),
            )
        result = response.get("result")
        if not isinstance(result, dict):
            raise HerdrTransportError("Herdr response did not contain an object result")
        return result


ProcessStarter = Callable[..., Any]


def herdr_socket_path(
    session_name: str,
    *,
    config_dir: Path | None = None,
    socket_path: Path | None = None,
) -> Path:
    """Resolve Herdr's default or named-session socket path."""
    if socket_path is not None:
        return socket_path.expanduser()
    if config_dir is not None:
        herdr_config = config_dir.expanduser()
    elif os.environ.get("HERDR_CONFIG_PATH"):
        herdr_config = Path(os.environ["HERDR_CONFIG_PATH"]).expanduser().parent
    else:
        herdr_config = Path.home() / ".config" / "herdr"

    if session_name == "default":
        override = os.environ.get("HERDR_SOCKET_PATH")
        return Path(override).expanduser() if override else herdr_config / "herdr.sock"
    return herdr_config / "sessions" / session_name / "herdr.sock"


class HerdrAdapter:
    """Create reusable Herdr pane slots and reconnect them after restart."""

    _MISSING_PANE_CODES = frozenset({"not_found", "pane_not_found", "unknown_pane"})

    def __init__(
        self,
        session_name: str,
        store: FactoryStore,
        *,
        socket_client: HerdrSocket | None = None,
        socket_path: Path | None = None,
        config_dir: Path | None = None,
        cli_executable: str = "herdr",
        process_starter: ProcessStarter | None = None,
        startup_timeout: float = 15.0,
        poll_interval: float = 0.1,
    ) -> None:
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,31}", session_name):
            raise ValueError("Herdr session name is invalid")
        if startup_timeout <= 0 or poll_interval <= 0:
            raise ValueError("Herdr startup timing values must be positive")
        self.session_name = session_name
        self.store = store
        self.cli_executable = cli_executable
        self._process_starter = process_starter or subprocess.Popen
        self.startup_timeout = startup_timeout
        self.poll_interval = poll_interval
        self._process: Any | None = None

        resolved_socket_path = herdr_socket_path(
            session_name,
            config_dir=config_dir,
            socket_path=socket_path,
        )
        self._socket = socket_client or HerdrSocketClient(resolved_socket_path)

    def _ensure_session(self) -> None:
        try:
            self._socket.call("ping", {})
            return
        except HerdrTransportError:
            pass

        if self._process is None:
            command = [
                self.cli_executable,
                "--session",
                self.session_name,
                "server",
            ]
            try:
                self._process = self._process_starter(
                    command,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    start_new_session=True,
                )
            except OSError as start_error:
                raise HerdrError(
                    "could not start the named Herdr server"
                ) from start_error

        deadline = time.monotonic() + self.startup_timeout
        last_error: HerdrTransportError | None = None
        while time.monotonic() < deadline:
            try:
                self._socket.call("ping", {})
                return
            except HerdrTransportError as connection_error:
                last_error = connection_error
                process_status = self._process.poll()
                if process_status not in (None, 0):
                    raise HerdrError(
                        f"Herdr server exited with status {process_status}"
                    ) from connection_error
                time.sleep(self.poll_interval)
        raise HerdrError("Herdr server did not become reachable") from last_error

    @staticmethod
    def _agent_name(worker_id: str) -> str:
        normalized = re.sub(r"[^a-z0-9_-]+", "-", worker_id.casefold()).strip("-_")
        if not normalized or not normalized[0].isalpha():
            normalized = f"worker-{normalized}"
        return f"factory-{normalized}"[:32]

    @staticmethod
    def _result_object(result: dict[str, Any], key: str) -> dict[str, Any]:
        value = result.get(key)
        if not isinstance(value, dict):
            raise HerdrError(f"Herdr response is missing {key}")
        return value

    def _pane_exists(self, pane_id: str) -> dict[str, Any] | None:
        try:
            result = self._socket.call("pane.get", {"pane_id": pane_id})
        except HerdrAPIError as api_error:
            if api_error.code in self._MISSING_PANE_CODES:
                return None
            raise
        return self._result_object(result, "pane")

    def _pane_matches_slot(
        self,
        slot: WorkerSlot,
        pane: dict[str, Any] | None,
    ) -> bool:
        if pane is None:
            return False
        if pane.get("agent") != self._agent_name(slot.worker_id):
            return False
        if (
            slot.workspace_id is not None
            and pane.get("workspace_id") != slot.workspace_id
        ):
            return False
        expected_state = (
            "idle" if slot.status is WorkerStatus.DONE else slot.status.value
        )
        return pane.get("agent_status") == expected_state

    def _create_slot_pane(self, worker_id: str, cwd: Path) -> tuple[str, str]:
        existing_slots = self.store.list_worker_slots(self.session_name)
        for slot in existing_slots:
            pane = self._pane_exists(slot.pane_id)
            if not self._pane_matches_slot(slot, pane) or slot.workspace_id is None:
                continue
            workspace_id = slot.workspace_id
            result = self._socket.call(
                "pane.split",
                {
                    "workspace_id": workspace_id,
                    "target_pane_id": slot.pane_id,
                    "direction": "right",
                    "ratio": 0.5,
                    "cwd": str(cwd),
                    "focus": False,
                },
            )
            pane_info = self._result_object(result, "pane")
            split_pane_id = pane_info.get("pane_id")
            if not isinstance(split_pane_id, str) or not split_pane_id:
                raise HerdrError("Herdr pane split returned no pane identifier")
            return workspace_id, split_pane_id

        workspace_result = self._socket.call(
            "workspace.create",
            {
                "label": f"Cronos AI ({self.session_name})",
                "cwd": str(cwd),
                "focus": False,
            },
        )
        workspace = self._result_object(workspace_result, "workspace")
        pane = self._result_object(workspace_result, "root_pane")
        root_workspace_id = workspace.get("workspace_id")
        root_pane_id = pane.get("pane_id")
        if not isinstance(root_workspace_id, str) or not isinstance(root_pane_id, str):
            raise HerdrError("Herdr workspace creation returned incomplete identifiers")
        return root_workspace_id, root_pane_id

    def ensure_slot(self, worker_id: str, cwd: Path) -> WorkerSlot:
        """Return a live persisted slot or create a pane for a new worker."""
        worktree = cwd.resolve(strict=True)
        if not worktree.is_dir():
            raise HerdrError("Herdr worker cwd must be an existing directory")
        self._ensure_session()

        existing = self.store.get_worker_slot(worker_id)
        if existing is not None:
            if existing.session_id != self.session_name:
                raise HerdrError("worker slot belongs to a different Herdr session")
            pane = self._pane_exists(existing.pane_id)
            if self._pane_matches_slot(existing, pane):
                return existing
            if existing.active_task_id is not None:
                raise HerdrError(
                    "active worker pane changed or disappeared; "
                    "refusing to duplicate it"
                )

        workspace_id, pane_id = self._create_slot_pane(worker_id, worktree)
        slot = WorkerSlot(
            worker_id=worker_id,
            session_id=self.session_name,
            workspace_id=workspace_id,
            pane_id=pane_id,
            status=WorkerStatus.IDLE,
        )
        self.store.save_worker_slot(slot)
        self.report_worker_status(worker_id, WorkerStatus.IDLE)
        return slot

    def reconcile_workers(self) -> list[ReconciledWorker]:
        """Reconnect live pane associations without creating replacement panes."""
        self._ensure_session()
        reconciled: list[ReconciledWorker] = []
        for slot in self.store.list_worker_slots(self.session_name):
            pane = self._pane_exists(slot.pane_id)
            live = self._pane_matches_slot(slot, pane)
            reconciled.append(ReconciledWorker(slot=slot, live=live))
        return reconciled

    def report_worker_status(
        self,
        worker_id: str,
        status: WorkerStatus,
        summary: str | None = None,
        *,
        task_id: str | None = None,
    ) -> WorkerSlot:
        """Persist and report structured lifecycle state for one Herdr pane."""
        slot = self.store.get_worker_slot(worker_id)
        if slot is None:
            raise HerdrError(f"worker slot does not exist: {worker_id}")
        active_task_id = slot.active_task_id
        if status in (WorkerStatus.IDLE, WorkerStatus.DONE):
            active_task_id = None
        elif task_id is not None:
            if not task_id.strip():
                raise HerdrError("task identifier must not be empty")
            active_task_id = task_id
        updated = slot.model_copy(
            update={
                "status": status,
                "active_task_id": active_task_id,
            }
        )
        self.store.save_worker_slot(updated)
        herdr_state = "idle" if status is WorkerStatus.DONE else status.value
        params: dict[str, Any] = {
            "pane_id": updated.pane_id,
            "source": f"factory:{self.session_name}",
            "agent": self._agent_name(updated.worker_id),
            "state": herdr_state,
        }
        if summary is not None:
            params["message"] = summary
        self._ensure_session()
        self._socket.call("pane.report_agent", params)
        metadata: dict[str, Any]
        if status is WorkerStatus.DONE:
            metadata = {
                "pane_id": updated.pane_id,
                "source": f"factory:{self.session_name}",
                "agent": self._agent_name(updated.worker_id),
                "state_labels": {"idle": "Done"},
            }
        else:
            metadata = {
                "pane_id": updated.pane_id,
                "source": f"factory:{self.session_name}",
                "agent": self._agent_name(updated.worker_id),
                "clear_state_labels": True,
            }
        self._socket.call("pane.report_metadata", metadata)
        return updated

    def report_task_state(
        self,
        worker_id: str,
        task_id: str,
        state: TaskState,
        summary: str,
        *,
        reason: str | None = None,
    ) -> WorkerSlot:
        """Map a fine-grained task state to an honest Herdr sidebar status."""
        if not task_id.strip():
            raise HerdrError("task identifier must not be empty")
        if state in (TaskState.RUNNING, TaskState.INTEGRATING):
            worker_status = WorkerStatus.WORKING
        elif state is TaskState.DONE:
            worker_status = WorkerStatus.DONE
        elif state in (
            TaskState.WAITING_FOR_HUMAN,
            TaskState.BLOCKED,
            TaskState.REVIEW,
            TaskState.FAILED,
        ):
            worker_status = WorkerStatus.BLOCKED
        else:
            worker_status = WorkerStatus.IDLE

        message = f"{task_id}: {' '.join(summary.split())}"
        if reason:
            message += f" - {' '.join(reason.split())}"
        if len(message) > 240:
            message = message[:237].rstrip() + "..."
        return self.report_worker_status(
            worker_id,
            worker_status,
            message,
            task_id=(
                task_id
                if worker_status in (WorkerStatus.WORKING, WorkerStatus.BLOCKED)
                else None
            ),
        )

    def assign_task(
        self,
        worker_id: str,
        task_id: str,
        summary: str,
    ) -> WorkerSlot:
        """Assign one task to an idle reusable Herdr worker slot."""
        slot = self.store.get_worker_slot(worker_id)
        if slot is None:
            raise HerdrError(f"worker slot does not exist: {worker_id}")
        if slot.status not in (WorkerStatus.IDLE, WorkerStatus.DONE):
            raise HerdrError("worker slot already has an active task")
        if not task_id.strip():
            raise HerdrError("task identifier must not be empty")
        if self.store.has_active_worker_for_task(
            task_id,
            excluding_worker_id=worker_id,
        ):
            raise HerdrError("task is already assigned to another worker")
        updated = slot.model_copy(
            update={
                "status": WorkerStatus.WORKING,
                "active_task_id": task_id,
            }
        )
        self.store.save_worker_slot(updated)
        return self.report_task_state(
            worker_id,
            task_id,
            TaskState.RUNNING,
            summary,
        )
