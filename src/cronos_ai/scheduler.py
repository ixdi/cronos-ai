"""LangGraph dependency scheduler backed by the SQLite execution ledger."""

from __future__ import annotations

from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import UTC, datetime
from threading import Lock
from typing import TypedDict, cast

from langgraph.graph import END, START, StateGraph

from cronos_ai.approval import ApprovalError, plan_fingerprint
from cronos_ai.models import (
    ApprovalDecision,
    Attempt,
    AttemptStatus,
    OpenSpecPlan,
    PlanTask,
    PlanTaskKind,
    RequestSource,
    RunContext,
    TaskRecord,
    TaskState,
    TriageOutcome,
    TriageResult,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore


class SchedulerError(RuntimeError):
    """Raised when a plan cannot be scheduled from its durable state."""


class TransientWorkerError(RuntimeError):
    """Marks a worker/runtime failure eligible for a bounded retry."""


class _SchedulerState(TypedDict):
    run_id: str
    ready_task_ids: list[str]


@dataclass(frozen=True)
class TaskAssignment:
    """A durable reservation of one ready task on one worker slot."""

    task: PlanTask
    worker_id: str
    attempt_number: int

    @property
    def task_id(self) -> str:
        return self.task.task_id


@dataclass(frozen=True)
class TaskDispatchResult:
    """Settled outcome for a reserved worker-slot assignment."""

    assignment: TaskAssignment
    success: bool
    reason: str | None = None


class BoundedTaskScheduler:
    """Reserve dependency-ready tasks within concurrency and slot limits."""

    def __init__(
        self,
        run_id: str,
        store: FactoryStore,
        graph: ExecutionGraph,
        *,
        worker_ids: Sequence[str],
        max_concurrency: int,
        max_attempts: int = 3,
    ) -> None:
        if max_concurrency < 1:
            raise ValueError("maximum scheduler concurrency must be positive")
        if max_attempts < 1:
            raise ValueError("maximum task attempts must be positive")
        if len(worker_ids) != len(set(worker_ids)):
            raise ValueError("worker slot identifiers must be unique")
        if any(not worker_id.strip() for worker_id in worker_ids):
            raise ValueError("worker slot identifiers must not be empty")
        self.run_id = run_id
        self.store = store
        self.graph = graph
        self.worker_ids = tuple(worker_ids)
        self.max_concurrency = max_concurrency
        self.max_attempts = max_attempts
        self._dispatch_lock = Lock()

    def dispatch_ready(self) -> list[TaskAssignment]:
        """Persist assignments only for dependency-ready tasks and idle slots."""
        with self._dispatch_lock:
            return self._reserve_ready_tasks()

    def _reserve_ready_tasks(self) -> list[TaskAssignment]:
        run = self.store.get_run(self.run_id)
        if run is None:
            raise SchedulerError(f"run does not exist: {self.run_id}")
        request, plan = run
        context = self.store.get_run_context(self.run_id)
        requires_approval = request.triage_outcome is TriageOutcome.SPECS_REQUIRED or (
            context is not None and context.approval_required
        )
        if requires_approval:
            if context is None or not context.approval_required:
                return []
            if context.plan_hash is None:
                return []
            try:
                current_hash = plan_fingerprint(
                    context.run_worktree_path
                    / "openspec"
                    / "changes"
                    / plan.change_name
                )
            except ApprovalError:
                return []
            if current_hash != context.plan_hash:
                return []
            approval = self.store.get_plan_approval(
                self.run_id, plan.change_name, current_hash
            )
            if approval is None or approval.decision is not ApprovalDecision.APPROVED:
                return []
        ready_ids = set(self.graph.refresh(self.run_id))
        if not ready_ids:
            return []

        records = self.store.list_tasks(self.run_id)
        records_by_id = {record.task_id: record for record in records}
        active_workers = self.store.active_worker_ids()
        available_workers = [
            worker_id
            for worker_id in self.worker_ids
            if worker_id not in active_workers
        ]
        capacity = max(0, self.max_concurrency - len(active_workers))
        assignment_count = min(capacity, len(available_workers))
        if assignment_count == 0:
            return []

        ready_tasks = [task for task in plan.tasks if task.task_id in ready_ids]
        selected = ready_tasks[:assignment_count]
        assignments: list[TaskAssignment] = []
        for task, worker_id in zip(selected, available_workers, strict=False):
            record = records_by_id.get(task.task_id)
            if record is None or record.state is not TaskState.READY:
                continue
            attempt_number = record.attempt_count + 1
            running_record = record.model_copy(
                update={
                    "state": TaskState.RUNNING,
                    "worker_id": worker_id,
                    "attempt_count": attempt_number,
                    "reason": None,
                }
            )
            attempt = Attempt(
                task_id=task.task_id,
                attempt_number=attempt_number,
                status=AttemptStatus.RUNNING,
                started_at=datetime.now(UTC),
                worker_id=worker_id,
            )
            self.store.record_attempt(self.run_id, running_record, attempt)
            assignments.append(
                TaskAssignment(
                    task=task,
                    worker_id=worker_id,
                    attempt_number=attempt_number,
                )
            )

        return assignments

    def _current_attempt(self, assignment: TaskAssignment) -> Attempt:
        attempts = self.store.get_attempts(self.run_id, assignment.task_id)
        attempt = next(
            (
                item
                for item in attempts
                if item.attempt_number == assignment.attempt_number
            ),
            None,
        )
        if attempt is None or attempt.status is not AttemptStatus.RUNNING:
            raise SchedulerError(
                f"running attempt is missing for task {assignment.task_id}"
            )
        return attempt

    def _finish_success(self, assignment: TaskAssignment) -> TaskDispatchResult:
        record = self.store.get_task(self.run_id, assignment.task_id)
        if record is None:
            raise SchedulerError(
                f"task disappeared during dispatch: {assignment.task_id}"
            )
        attempt = self._current_attempt(assignment)
        next_state = (
            TaskState.INTEGRATING if record.state is TaskState.RUNNING else record.state
        )
        updated_task = record.model_copy(
            update={
                "state": next_state,
                "worker_id": None,
                "reason": (
                    None if next_state is TaskState.INTEGRATING else record.reason
                ),
            }
        )
        finished_attempt = attempt.model_copy(
            update={
                "status": AttemptStatus.SUCCEEDED,
                "finished_at": datetime.now(UTC),
            }
        )
        self.store.finish_attempt(self.run_id, updated_task, finished_attempt)
        success = next_state not in (TaskState.BLOCKED, TaskState.FAILED)
        return TaskDispatchResult(
            assignment=assignment,
            success=success,
            reason=updated_task.reason,
        )

    def _finish_failure(
        self,
        assignment: TaskAssignment,
        error: Exception,
    ) -> TaskDispatchResult:
        record = self.store.get_task(self.run_id, assignment.task_id)
        if record is None:
            raise SchedulerError(
                f"task disappeared during dispatch: {assignment.task_id}"
            ) from error
        attempt = self._current_attempt(assignment)
        reason = str(error).strip()[:1000] or type(error).__name__
        transient = isinstance(error, TransientWorkerError) or bool(
            getattr(error, "transient", False)
        )
        retry = (
            transient
            and assignment.attempt_number < self.max_attempts
            and record.state is TaskState.RUNNING
        )
        if retry:
            next_state = TaskState.READY
            next_reason = None
        elif record.state in (TaskState.BLOCKED, TaskState.DONE):
            next_state = record.state
            next_reason = record.reason
        else:
            next_state = TaskState.FAILED
            next_reason = reason

        updated_task = record.model_copy(
            update={
                "state": next_state,
                "worker_id": None,
                "reason": next_reason,
            }
        )
        finished_attempt = attempt.model_copy(
            update={
                "status": AttemptStatus.FAILED,
                "finished_at": datetime.now(UTC),
                "reason": reason,
                "transient": transient,
            }
        )
        self.store.finish_attempt(self.run_id, updated_task, finished_attempt)
        return TaskDispatchResult(
            assignment=assignment,
            success=False,
            reason=reason,
        )

    def run_ready(
        self,
        dispatch: Callable[[TaskAssignment], object],
    ) -> list[TaskDispatchResult]:
        """Dispatch ready tasks concurrently, retrying only classified transients."""
        outcomes: list[TaskDispatchResult] = []
        while assignments := self.dispatch_ready():
            completed: dict[str, TaskDispatchResult] = {}
            with ThreadPoolExecutor(max_workers=len(assignments)) as executor:
                futures = {
                    executor.submit(dispatch, assignment): assignment
                    for assignment in assignments
                }
                for future in as_completed(futures):
                    assignment = futures[future]
                    try:
                        future.result()
                    except Exception as error:
                        completed[assignment.task_id] = self._finish_failure(
                            assignment,
                            error,
                        )
                    else:
                        completed[assignment.task_id] = self._finish_success(assignment)
            outcomes.extend(completed[assignment.task_id] for assignment in assignments)
        return outcomes


class ExecutionGraph:
    """Schedule dependency-ready OpenSpec tasks through LangGraph."""

    def __init__(self, store: FactoryStore) -> None:
        self.store = store
        graph = StateGraph(_SchedulerState)
        graph.add_node("refresh_dependencies", self._refresh_dependencies)
        graph.add_edge(START, "refresh_dependencies")
        graph.add_edge("refresh_dependencies", END)
        self._graph = graph.compile()

    def initialize_run(
        self,
        run_id: str,
        request: WorkRequest,
        plan: OpenSpecPlan,
        *,
        run_context: RunContext | None = None,
        triage_result: TriageResult | None = None,
    ) -> None:
        """Persist a plan, its task ledger, and optional durable run context."""
        if request.source is RequestSource.MONITORING and triage_result is None:
            raise SchedulerError("monitoring alerts require ordinary triage")
        if request.triage_outcome is not None and triage_result is None:
            raise SchedulerError("triaged requests require their triage result")
        if triage_result is not None:
            if triage_result.request != request:
                raise SchedulerError("triage result does not match the run request")
            if not triage_result.can_plan:
                raise SchedulerError("request triage outcome does not permit planning")
            if triage_result.requires_approval and (
                run_context is None or not run_context.approval_required
            ):
                raise SchedulerError(
                    "high-impact triage requires a durable approval context"
                )
        if request.triage_outcome is TriageOutcome.SPECS_REQUIRED and (
            run_context is None or not run_context.approval_required
        ):
            raise SchedulerError(
                "specifications-required runs need a durable approval context"
            )
        if run_context is not None and run_context.run_id != run_id:
            raise SchedulerError("run context identifier does not match run id")
        if run_context is not None and triage_result is not None:
            if run_context.triage is not None and run_context.triage != triage_result:
                raise SchedulerError("run context contains a different triage result")
            if run_context.triage is None:
                run_context = run_context.model_copy(update={"triage": triage_result})
        approval_required = (
            run_context.approval_required if run_context is not None else False
        )
        initial_tasks = tuple(
            TaskRecord(
                task_id=task.task_id,
                state=(
                    TaskState.DONE
                    if task.kind is PlanTaskKind.PLANNING
                    or (task.kind is PlanTaskKind.APPROVAL and not approval_required)
                    else TaskState.WAITING_FOR_HUMAN
                    if task.kind is PlanTaskKind.APPROVAL
                    else TaskState.QUEUED
                ),
                reason=(
                    "Detailed OpenSpec plan approval is required."
                    if task.kind is PlanTaskKind.APPROVAL and approval_required
                    else None
                ),
            )
            for task in plan.tasks
        )
        self.store.create_run(run_id, request, plan, initial_tasks)
        if run_context is not None:
            self.store.save_run_context(run_context)

    def refresh(self, run_id: str) -> list[str]:
        """Run one graph transition and return newly dependency-ready tasks."""
        state = cast(
            _SchedulerState,
            self._graph.invoke({"run_id": run_id, "ready_task_ids": []}),
        )
        return state["ready_task_ids"]

    def _refresh_dependencies(self, state: _SchedulerState) -> _SchedulerState:
        run = self.store.get_run(state["run_id"])
        if run is None:
            raise SchedulerError(f"run does not exist: {state['run_id']}")
        _, plan = run

        records = self.store.list_tasks(state["run_id"])
        record_by_id = {record.task_id: record for record in records}
        integrated_ids = {
            record.task_id
            for record in records
            if record.state in (TaskState.REVIEW, TaskState.DONE)
        }
        updates: list[TaskRecord] = []
        for task in plan.tasks:
            current = record_by_id.get(task.task_id)
            if current is None:
                current = TaskRecord(task_id=task.task_id, state=TaskState.QUEUED)
            if current.state not in (TaskState.QUEUED, TaskState.READY):
                continue
            next_state = (
                TaskState.READY
                if all(dependency in integrated_ids for dependency in task.depends_on)
                else TaskState.QUEUED
            )
            if current.state is not next_state:
                current = current.model_copy(update={"state": next_state})
                updates.append(current)
            record_by_id[task.task_id] = current

        if updates:
            self.store.save_tasks(state["run_id"], tuple(updates))

        ready_task_ids = [
            task.task_id
            for task in plan.tasks
            if record_by_id[task.task_id].state is TaskState.READY
        ]
        return {"run_id": state["run_id"], "ready_task_ids": ready_task_ids}
