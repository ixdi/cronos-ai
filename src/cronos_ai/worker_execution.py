"""Coordinate one task across Herdr, Pi RPC, and an isolated worktree."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

from cronos_ai.activity import sanitize_activity_summary
from cronos_ai.herdr import HerdrAdapter, HerdrTransportError
from cronos_ai.models import ActivityEvent, SpecialistProfile, TaskState, WorkerSlot
from cronos_ai.pi_rpc import (
    PiProcessExited,
    PiRpcSupervisor,
    PiRunResult,
    PiRunTimeout,
    ProcessFactory,
)
from cronos_ai.profiles import ResolvedSpecialistProfile
from cronos_ai.sandbox import DockerSandboxAdapter, SandboxError
from cronos_ai.scheduler import TransientWorkerError


class WorkerExecutionError(RuntimeError):
    """Raised when an isolated worker cannot settle a task permanently."""


@dataclass(frozen=True)
class WorkerExecutionResult:
    """A settled Pi result and the corresponding released Herdr slot."""

    task_id: str
    slot: WorkerSlot
    rpc_result: PiRunResult


class PiWorkerExecutor:
    """Run one task through a reusable Herdr slot and sandboxed Pi RPC worker."""

    def __init__(
        self,
        herdr: HerdrAdapter,
        sandbox: DockerSandboxAdapter,
        profile: SpecialistProfile,
        *,
        resources: ResolvedSpecialistProfile | None = None,
        pi_executable: str = "pi",
        command: Sequence[str] | None = None,
        process_factory: ProcessFactory | None = None,
    ) -> None:
        self.herdr = herdr
        self.sandbox = sandbox
        self.profile = profile
        self.resources = resources
        self.pi_executable = pi_executable
        self.command = tuple(command) if command is not None else None
        self.process_factory = process_factory

    @staticmethod
    def _progress_summary(event: dict[str, object]) -> str | None:
        event_type = event.get("type")
        if event_type == "tool_execution_start":
            tool_name = event.get("toolName")
            if isinstance(tool_name, str):
                return f"Running {tool_name}"
        if event_type == "message_end":
            message = event.get("message")
            if isinstance(message, dict) and message.get("role") == "assistant":
                content = message.get("content")
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            text = block.get("text")
                            if isinstance(text, str) and text.strip():
                                return text.strip()[:160]
        return None

    def execute(
        self,
        worktree: Path,
        *,
        run_id: str,
        worker_id: str,
        task_id: str,
        prompt: str,
        timeout: float = 1800.0,
    ) -> WorkerExecutionResult:
        """Run the task, forward concise progress, and wait for agent_settled."""
        assigned = False

        def report_progress(event: dict[str, object]) -> None:
            summary = self._progress_summary(event)
            if summary is None:
                return
            safe_summary = sanitize_activity_summary(summary)
            if safe_summary is None:
                return
            self.herdr.store.append_activity_event(
                ActivityEvent(
                    event_id=str(uuid4()),
                    run_id=run_id,
                    task_id=task_id,
                    occurred_at=datetime.now(UTC),
                    category="progress",
                    summary=safe_summary,
                )
            )
            self.herdr.report_task_state(
                worker_id,
                task_id,
                TaskState.RUNNING,
                safe_summary,
            )

        supervisor: PiRpcSupervisor | None = None
        result: PiRunResult | None = None
        failure: Exception | None = None
        try:
            self.herdr.ensure_slot(worker_id, worktree)
            self.herdr.assign_task(worker_id, task_id, prompt)
            assigned = True
            supervisor = PiRpcSupervisor(
                self.profile,
                worktree,
                resources=self.resources,
                executable=self.pi_executable,
                command=self.command,
                process_factory=(
                    self.process_factory
                    or self.sandbox.process_factory(worktree, self.profile)
                ),
                event_handler=report_progress,
            )
            result = supervisor.run_prompt(prompt, timeout=timeout)
        except Exception as error:
            failure = error
        finally:
            if supervisor is not None:
                try:
                    supervisor.close()
                except Exception as close_error:
                    if failure is None:
                        failure = close_error

        if failure is not None:
            transient = isinstance(
                failure,
                (PiProcessExited, PiRunTimeout, HerdrTransportError),
            ) or (isinstance(failure, SandboxError) and failure.transient)
            if assigned:
                try:
                    self.herdr.report_task_state(
                        worker_id,
                        task_id,
                        TaskState.READY if transient else TaskState.BLOCKED,
                        "Transient worker failure; retry is eligible"
                        if transient
                        else "Pi worker failed before reporting agent_settled",
                    )
                except Exception:
                    pass
            if transient:
                raise TransientWorkerError(
                    f"transient Pi worker failure for task {task_id}"
                ) from failure
            raise WorkerExecutionError(f"worker failed for task {task_id}") from failure
        if result is None or not result.settled:
            self.herdr.report_task_state(
                worker_id,
                task_id,
                TaskState.BLOCKED,
                "Pi worker did not report agent_settled",
            )
            raise WorkerExecutionError(f"worker did not settle task {task_id}")

        completed_slot = self.herdr.report_task_state(
            worker_id,
            task_id,
            TaskState.DONE,
            "Task implementation settled",
        )
        return WorkerExecutionResult(
            task_id=task_id,
            slot=completed_slot,
            rpc_result=result,
        )
