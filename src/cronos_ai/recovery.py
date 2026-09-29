"""Controller restart reconciliation and bounded task recovery."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum

from cronos_ai.herdr import HerdrAdapter
from cronos_ai.models import (
    Attempt,
    AttemptStatus,
    TaskRecord,
    TaskState,
    WorkerSlot,
    WorkerStatus,
)
from cronos_ai.storage import FactoryStore


class RecoveryError(RuntimeError):
    """Raised when persisted task state cannot be safely reconciled."""


class RecoveryDisposition(StrEnum):
    RECONNECTED = "reconnected"
    WAITING_FOR_HUMAN = "waiting-for-human"
    RETRY = "retry"
    FAILED = "failed"
    BLOCKED = "blocked"


@dataclass(frozen=True)
class RecoveryResult:
    """A task and its controller restart recovery decision."""

    task_id: str
    disposition: RecoveryDisposition
    slot: WorkerSlot | None = None
    reason: str | None = None


class TaskRecoveryManager:
    """Reconcile persisted worker associations before replacement dispatch."""

    def __init__(self, store: FactoryStore, *, max_attempts: int) -> None:
        if max_attempts < 1:
            raise ValueError("maximum task attempts must be positive")
        self.store = store
        self.max_attempts = max_attempts

    def recover(
        self,
        run_id: str,
        herdr: HerdrAdapter,
    ) -> list[RecoveryResult]:
        """Reconnect live workers or interrupt their attempts with bounded retry."""
        if self.store.get_run(run_id) is None:
            raise RecoveryError(f"run does not exist: {run_id}")
        reconciled = {
            worker.slot.worker_id: worker
            for worker in herdr.reconcile_workers()
        }
        results: list[RecoveryResult] = []
        for task in self.store.list_tasks(run_id):
            if task.state is not TaskState.RUNNING:
                continue
            if task.worker_id is None:
                results.append(
                    self._block_inconsistent_task(
                        run_id,
                        task,
                        "running task has no worker",
                    )
                )
                continue

            worker = reconciled.get(task.worker_id)
            slot = worker.slot if worker is not None else None
            if (
                worker is not None
                and worker.live
                and slot is not None
                and slot.active_task_id == task.task_id
                and slot.status is WorkerStatus.WORKING
            ):
                results.append(
                    RecoveryResult(
                        task_id=task.task_id,
                        disposition=RecoveryDisposition.RECONNECTED,
                        slot=slot,
                    )
                )
                continue

            if (
                worker is not None
                and worker.live
                and slot is not None
                and slot.active_task_id == task.task_id
                and slot.status is WorkerStatus.BLOCKED
            ):
                reason = "worker is waiting for a human in Herdr"
                waiting = task.model_copy(
                    update={
                        "state": TaskState.WAITING_FOR_HUMAN,
                        "reason": reason,
                    }
                )
                self.store.save_task(run_id, waiting)
                results.append(
                    RecoveryResult(
                        task_id=task.task_id,
                        disposition=RecoveryDisposition.WAITING_FOR_HUMAN,
                        slot=slot,
                        reason=reason,
                    )
                )
                continue

            if slot is not None and slot.active_task_id not in (None, task.task_id):
                reason = "worker slot association does not match the running task"
                results.append(self._interrupt(run_id, task, reason, blocked=True))
                continue

            reason = "worker did not survive controller restart"
            retry = task.attempt_count < self.max_attempts
            recovered_task = task.model_copy(
                update={
                    "state": TaskState.READY if retry else TaskState.FAILED,
                    "worker_id": None,
                    "reason": None if retry else f"{reason}; retry limit exhausted",
                }
            )
            interrupted = self._running_attempt(run_id, task)
            final_attempt = interrupted.model_copy(
                update={
                    "status": AttemptStatus.INTERRUPTED,
                    "finished_at": datetime.now(UTC),
                    "reason": reason,
                }
            )
            slot_update = None
            if slot is not None and slot.active_task_id == task.task_id:
                slot_update = slot.model_copy(
                    update={
                        "status": WorkerStatus.IDLE,
                        "active_task_id": None,
                    }
                )
            self.store.finish_attempt(
                run_id,
                recovered_task,
                final_attempt,
                worker_slot_update=slot_update,
            )
            results.append(
                RecoveryResult(
                    task_id=task.task_id,
                    disposition=(
                        RecoveryDisposition.RETRY
                        if retry
                        else RecoveryDisposition.FAILED
                    ),
                    slot=slot_update or slot,
                    reason=reason,
                )
            )
        return results

    def _running_attempt(self, run_id: str, task: TaskRecord) -> Attempt:
        attempts = self.store.get_attempts(run_id, task.task_id)
        attempt = next(
            (
                item
                for item in attempts
                if item.attempt_number == task.attempt_count
                and item.status is AttemptStatus.RUNNING
            ),
            None,
        )
        if attempt is None:
            raise RecoveryError(
                f"running task has no matching persisted attempt: {task.task_id}"
            )
        return attempt

    def _interrupt(
        self,
        run_id: str,
        task: TaskRecord,
        reason: str,
        *,
        blocked: bool,
    ) -> RecoveryResult:
        attempt = self._running_attempt(run_id, task)
        finished_attempt = attempt.model_copy(
            update={
                "status": AttemptStatus.INTERRUPTED,
                "finished_at": datetime.now(UTC),
                "reason": reason,
            }
        )
        blocked_task = task.model_copy(
            update={
                "state": TaskState.BLOCKED if blocked else TaskState.FAILED,
                "worker_id": None,
                "reason": reason,
            }
        )
        self.store.finish_attempt(run_id, blocked_task, finished_attempt)
        return RecoveryResult(
            task_id=task.task_id,
            disposition=RecoveryDisposition.BLOCKED,
            reason=reason,
        )

    def _block_inconsistent_task(
        self,
        run_id: str,
        task: TaskRecord,
        reason: str,
    ) -> RecoveryResult:
        attempt = self._running_attempt(run_id, task)
        finished_attempt = attempt.model_copy(
            update={
                "status": AttemptStatus.INTERRUPTED,
                "finished_at": datetime.now(UTC),
                "reason": reason,
            }
        )
        blocked_task = task.model_copy(
            update={
                "state": TaskState.BLOCKED,
                "worker_id": None,
                "reason": reason,
            }
        )
        self.store.finish_attempt(run_id, blocked_task, finished_attempt)
        return RecoveryResult(
            task_id=task.task_id,
            disposition=RecoveryDisposition.BLOCKED,
            reason=reason,
        )
