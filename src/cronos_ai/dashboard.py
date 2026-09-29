"""Read-only dashboard snapshots composed from durable factory state."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from cronos_ai.activity import sanitize_activity_summary
from cronos_ai.attention import HumanAttentionQueue
from cronos_ai.models import (
    ActivityEvent,
    Attempt,
    AttemptStatus,
    AttentionItem,
    ControlActionType,
    ControllerStatus,
    OpenSpecPlan,
    PlanTask,
    RunContext,
    TaskRecord,
    TaskState,
)
from cronos_ai.storage import FactoryStore


@dataclass(frozen=True)
class DashboardAttempt:
    """Safe summary of one persisted task attempt."""

    attempt_number: int
    status: AttemptStatus
    started_at: str
    finished_at: str | None
    worker_id: str | None
    reason: str | None


@dataclass(frozen=True)
class DashboardTask:
    """Plan details combined with the latest persisted task state."""

    task_id: str
    description: str
    state: TaskState | None
    attempt_count: int
    worker_id: str | None
    reason: str | None
    attempts: tuple[DashboardAttempt, ...]


@dataclass(frozen=True)
class DashboardAttention:
    """Sanitized, display-only human-attention information."""

    item_id: str
    run_id: str | None
    state: str
    reason: str
    latest_result: str | None
    available_actions: tuple[ControlActionType, ...]
    recommended_action: str | None


@dataclass(frozen=True)
class DashboardRun:
    """Bounded monitoring summary and details for one factory run."""

    run_id: str
    request_summary: str
    state: str
    current_stage: str
    task_counts: dict[str, int]
    completed_tasks: int
    total_tasks: int
    completion_percentage: int | None
    worker_assignments: tuple[tuple[str, str], ...]
    tasks: tuple[DashboardTask, ...]
    attention_items: tuple[DashboardAttention, ...]
    activity_events: tuple[ActivityEvent, ...]
    activity_total_events: int
    activity_page_count: int
    worktree_path: Path | None
    open_spec_path: Path | None
    review_state: str | None
    review_summary: str | None


@dataclass(frozen=True)
class ActivityPage:
    """One bounded slice of a run's newest-first activity history."""

    events: tuple[ActivityEvent, ...]
    offset: int
    total_events: int
    page_number: int
    page_count: int


@dataclass(frozen=True)
class DashboardSnapshot:
    """A point-in-time, read-only view of the local factory ledger."""

    controller_status: ControllerStatus | None
    runs: tuple[DashboardRun, ...]
    attention_items: tuple[DashboardAttention, ...]


class DashboardReadModel:
    """Assemble bounded dashboard data without changing factory state."""

    def __init__(self, store: FactoryStore, *, events_per_run: int = 100) -> None:
        if not 1 <= events_per_run <= 1000:
            raise ValueError("events per run must be between 1 and 1000")
        self.store = store
        self.events_per_run = events_per_run

    def activity_page(self, run_id: str, *, offset: int = 0) -> ActivityPage:
        """Load one bounded page and its navigable history position."""
        if offset < 0:
            raise ValueError("activity page offset must not be negative")
        total_events = self.store.count_activity_events(run_id)
        page_count = max(
            1,
            (total_events + self.events_per_run - 1) // self.events_per_run,
        )
        page_number = min(offset // self.events_per_run + 1, page_count)
        page_offset = (page_number - 1) * self.events_per_run
        events = tuple(
            event.model_copy(update={"summary": safe_summary})
            for event in self.store.list_activity_events(
                run_id,
                limit=self.events_per_run,
                offset=page_offset,
            )
            if (safe_summary := sanitize_activity_summary(event.summary)) is not None
        )
        return ActivityPage(
            events=events,
            offset=page_offset,
            total_events=total_events,
            page_number=page_number,
            page_count=page_count,
        )

    def snapshot(self) -> DashboardSnapshot:
        """Read current controller, run, task, attention, and event summaries."""
        attention_items = tuple(
            self._attention(item)
            for item in HumanAttentionQueue(self.store).list_items()
        )
        attention_by_run: dict[str, list[DashboardAttention]] = {}
        for item in attention_items:
            if item.run_id is not None:
                attention_by_run.setdefault(item.run_id, []).append(item)

        runs: list[DashboardRun] = []
        for run_id in self.store.list_run_ids():
            run_data = self.store.get_run(run_id)
            if run_data is None:
                continue
            request, plan = run_data
            context = self.store.get_run_context(run_id)
            records = self.store.list_tasks(run_id)
            tasks_by_id = {record.task_id: record for record in records}
            planned_ids = {task.task_id for task in plan.tasks}
            projected_tasks = tuple(
                self._task(run_id, plan_task, tasks_by_id.get(plan_task.task_id))
                for plan_task in plan.tasks
            )
            run_attention = tuple(attention_by_run.get(run_id, ()))
            completed = sum(
                task.state is TaskState.DONE for task in projected_tasks
            )
            total = len(plan.tasks)
            worker_assignments = tuple(
                (task.task_id, task.worker_id)
                for task in projected_tasks
                if task.worker_id is not None
            )
            task_counts = {state.value: 0 for state in TaskState}
            task_counts["unknown"] = 0
            for task in projected_tasks:
                state = task.state.value if task.state is not None else "unknown"
                task_counts[state] += 1
            activity_page = self.activity_page(run_id)
            activity_events = activity_page.events
            review = self.store.get_latest_review_packet(run_id)
            runs.append(
                DashboardRun(
                    run_id=run_id,
                    request_summary=(
                        sanitize_activity_summary(request.description) or "[hidden]"
                    ),
                    state=self._run_state(projected_tasks, run_attention),
                    current_stage=self._stage(
                        plan.tasks,
                        projected_tasks,
                        records,
                        planned_ids,
                    ),
                    task_counts=task_counts,
                    completed_tasks=completed,
                    total_tasks=total,
                    completion_percentage=(
                        (completed * 100 + total // 2) // total if total else None
                    ),
                    worker_assignments=worker_assignments,
                    tasks=projected_tasks,
                    attention_items=run_attention,
                    activity_events=activity_events,
                    activity_total_events=activity_page.total_events,
                    activity_page_count=activity_page.page_count,
                    worktree_path=context.run_worktree_path if context else None,
                    open_spec_path=self._open_spec_path(context, plan)
                    if context
                    else None,
                    review_state=review.state.value if review else None,
                    review_summary=(
                        sanitize_activity_summary(review.review_summary)
                        if review is not None
                        else None
                    ),
                )
            )

        return DashboardSnapshot(
            controller_status=self.store.get_controller_status(),
            runs=tuple(runs),
            attention_items=attention_items,
        )

    def _task(
        self,
        run_id: str,
        plan_task: PlanTask,
        record: TaskRecord | None,
    ) -> DashboardTask:
        attempts: tuple[DashboardAttempt, ...] = ()
        if record is not None:
            attempts = tuple(
                self._attempt(attempt)
                for attempt in self.store.get_attempts(run_id, plan_task.task_id)
            )
        return DashboardTask(
            task_id=plan_task.task_id,
            description=sanitize_activity_summary(plan_task.description) or "[hidden]",
            state=record.state if record is not None else None,
            attempt_count=record.attempt_count if record is not None else 0,
            worker_id=record.worker_id if record is not None else None,
            reason=(
                sanitize_activity_summary(record.reason)
                if record is not None and record.reason is not None
                else None
            ),
            attempts=attempts,
        )

    @staticmethod
    def _attempt(attempt: Attempt) -> DashboardAttempt:
        return DashboardAttempt(
            attempt_number=attempt.attempt_number,
            status=attempt.status,
            started_at=attempt.started_at.isoformat(),
            finished_at=(
                attempt.finished_at.isoformat()
                if attempt.finished_at is not None
                else None
            ),
            worker_id=attempt.worker_id,
            reason=(
                sanitize_activity_summary(attempt.reason)
                if attempt.reason is not None
                else None
            ),
        )

    @staticmethod
    def _attention(item: AttentionItem) -> DashboardAttention:
        return DashboardAttention(
            item_id=item.item_id,
            run_id=item.run_id,
            state=item.state.value,
            reason=sanitize_activity_summary(item.reason) or "[hidden]",
            latest_result=(
                sanitize_activity_summary(item.latest_result)
                if item.latest_result is not None
                else None
            ),
            available_actions=item.available_actions,
            recommended_action=(
                sanitize_activity_summary(item.recommended_action)
                if item.recommended_action is not None
                else None
            ),
        )

    @staticmethod
    def _run_state(
        tasks: tuple[DashboardTask, ...],
        attention_items: tuple[DashboardAttention, ...],
    ) -> str:
        if any(item.available_actions for item in attention_items):
            return "awaiting human"
        states = {task.state for task in tasks if task.state is not None}
        if TaskState.BLOCKED in states:
            return "blocked"
        if TaskState.FAILED in states:
            return "failed"
        if states & {TaskState.RUNNING, TaskState.INTEGRATING}:
            return "running"
        if TaskState.WAITING_FOR_HUMAN in states:
            return "awaiting human"
        if TaskState.REVIEW in states:
            return "review"
        if tasks and all(task.state is TaskState.DONE for task in tasks):
            return "complete"
        if states & {TaskState.QUEUED, TaskState.READY}:
            return "queued"
        return "unknown"

    @staticmethod
    def _stage(
        plan_tasks: tuple[PlanTask, ...],
        projected_tasks: tuple[DashboardTask, ...],
        records: list[TaskRecord],
        planned_ids: set[str],
    ) -> str:
        planned_by_id = {task.task_id: task for task in plan_tasks}
        active_records = [
            record
            for record in records
            if record.state in (TaskState.RUNNING, TaskState.INTEGRATING)
        ]
        if any(record.task_id not in planned_ids for record in active_records):
            return "unknown"
        active_kinds = {
            planned_by_id[record.task_id].kind
            for record in active_records
            if record.task_id in planned_by_id
        }
        if len(active_kinds) == 1:
            return next(iter(active_kinds)).value
        if active_kinds:
            return "multiple stages"
        next_task = next(
            (task for task in projected_tasks if task.state is not TaskState.DONE),
            None,
        )
        if next_task is not None:
            plan_task = planned_by_id.get(next_task.task_id)
            return plan_task.kind.value if plan_task is not None else "unknown"
        return "complete" if plan_tasks else "unknown"

    @staticmethod
    def _open_spec_path(context: RunContext, plan: OpenSpecPlan) -> Path:
        return (
            context.run_worktree_path
            / "openspec"
            / "changes"
            / plan.change_name
        )
