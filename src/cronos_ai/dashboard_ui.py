"""Interactive read-only terminal dashboard for factory operations."""

from __future__ import annotations

from textual import work
from textual.app import App, ComposeResult
from textual.containers import VerticalScroll
from textual.widgets import DataTable, Footer, Header, Static

from cronos_ai.activity import sanitize_activity_summary
from cronos_ai.dashboard import (
    ActivityPage,
    DashboardAttention,
    DashboardReadModel,
    DashboardRun,
    DashboardSnapshot,
    DashboardTask,
)
from cronos_ai.models import ActivityEvent


class FactoryDashboardApp(App[None]):
    """Display refreshable factory status without mutating factory state."""

    TITLE = "Cronos AI | Factory Dashboard"
    CSS = """
    Screen {
        background: $surface;
    }

    #dashboard-content {
        height: 1fr;
        padding: 0 1;
    }

    .section-title {
        height: 1;
        margin-top: 1;
        text-style: bold;
        color: $accent;
    }

    #runs-table {
        height: 10;
        min-height: 5;
        border: round $primary;
    }

    #tasks-table {
        height: 8;
        min-height: 4;
        border: round $primary;
    }

    .detail-panel {
        height: auto;
        min-height: 3;
        padding: 0 1;
        border: round $secondary;
    }

    #empty-state {
        height: auto;
        padding: 1;
        color: $text-muted;
        border: round $warning;
    }

    #refresh-error {
        height: auto;
        color: $error;
    }

    """
    BINDINGS = [
        ("q", "quit", "Quit"),
        ("r", "refresh_data", "Refresh"),
        ("n", "next_activity_page", "Older activity"),
        ("p", "previous_activity_page", "Newer activity"),
    ]

    def __init__(
        self,
        read_model: DashboardReadModel,
        *,
        refresh_interval: float = 2.0,
    ) -> None:
        if refresh_interval <= 0:
            raise ValueError("dashboard refresh interval must be positive")
        super().__init__()
        self.read_model = read_model
        self.refresh_interval = refresh_interval
        self._snapshot: DashboardSnapshot | None = None
        self._runs_by_id: dict[str, DashboardRun] = {}
        self._selected_run_id: str | None = None
        self._selected_task_id: str | None = None
        self._activity_offset = 0
        self._activity_total_events = 0
        self._activity_page: ActivityPage | None = None
        self._current_tasks: dict[str, DashboardTask] = {}

    def compose(self) -> ComposeResult:
        yield Header()
        with VerticalScroll(id="dashboard-content"):
            yield Static("Controller: checking…", id="controller-status", markup=False)
            yield Static("", id="refresh-error", markup=False)
            yield Static("Factory runs", classes="section-title", markup=False)
            yield Static("", id="empty-state", markup=False)
            yield DataTable(id="runs-table", cursor_type="row", zebra_stripes=True)
            yield Static("Selected factory", classes="section-title", markup=False)
            yield Static("Select a factory run", id="selected-run", markup=False)
            yield Static("Tasks", classes="section-title", markup=False)
            yield DataTable(id="tasks-table", cursor_type="row", zebra_stripes=True)
            yield Static(
                "Select a task",
                id="task-details",
                classes="detail-panel",
                markup=False,
            )
            yield Static("Activity", classes="section-title", markup=False)
            yield Static("", id="activity-page", markup=False)
            yield Static(
                "No activity recorded.",
                id="activity-log",
                classes="detail-panel",
                markup=False,
            )
            yield Static("Human attention", classes="section-title", markup=False)
            yield Static(
                "No pending human decisions.",
                id="attention-list",
                classes="detail-panel",
                markup=False,
            )
            yield Static("Generated outputs", classes="section-title", markup=False)
            yield Static(
                "No outputs selected.",
                id="outputs",
                classes="detail-panel",
                markup=False,
            )
            yield Static("Review", classes="section-title", markup=False)
            yield Static(
                "Review summary not available.",
                id="review-summary",
                classes="detail-panel",
                markup=False,
            )
        yield Footer()

    def on_mount(self) -> None:
        runs_table = self.query_one("#runs-table", DataTable)
        runs_table.add_column("Factory run", key="run", width=28)
        runs_table.add_column("State", key="state", width=18)
        runs_table.add_column("Workflow stage", key="stage", width=18)
        runs_table.add_column("Tasks complete", key="progress", width=16)

        tasks_table = self.query_one("#tasks-table", DataTable)
        tasks_table.add_column("Task", key="task", width=28)
        tasks_table.add_column("State", key="state", width=18)
        tasks_table.add_column("Worker", key="worker", width=20)
        tasks_table.add_column("Attempts", key="attempts", width=10)

        self._refresh_snapshot()
        self.set_interval(self.refresh_interval, self._refresh_snapshot)

    @work(thread=True, exclusive=True, group="dashboard-snapshot")
    def _refresh_snapshot(self) -> None:
        try:
            snapshot = self.read_model.snapshot()
        except Exception as error:
            message = (
                sanitize_activity_summary(str(error)) or "Unable to read factory state."
            )
            self.call_from_thread(self._show_refresh_error, message)
            return
        self.call_from_thread(self._apply_snapshot, snapshot)

    def _show_refresh_error(self, message: str) -> None:
        self.query_one("#refresh-error", Static).update(
            f"Dashboard refresh failed: {message}"
        )

    def _apply_snapshot(self, snapshot: DashboardSnapshot) -> None:
        self._snapshot = snapshot
        self._runs_by_id = {run.run_id: run for run in snapshot.runs}
        status = snapshot.controller_status
        if status is None:
            controller_label = "Controller: not started"
        else:
            controller_label = (
                f"Controller: {status.state.value} | PID {status.pid} | "
                f"heartbeat {status.heartbeat_at.astimezone().strftime('%H:%M:%S')}"
            )
        self.query_one("#controller-status", Static).update(controller_label)
        self.query_one("#refresh-error", Static).update("")

        empty_state = self.query_one("#empty-state", Static)
        runs_table = self.query_one("#runs-table", DataTable)
        runs_table.clear(columns=False)
        empty_state.display = not snapshot.runs
        empty_state.update(
            "No factory runs yet. Submit work with `cronos-ai run --repo PATH "
            "--request 'Describe the work'`."
        )
        for run in snapshot.runs:
            progress = (
                f"{run.completed_tasks}/{run.total_tasks} "
                f"({run.completion_percentage}%)"
                if run.completion_percentage is not None
                else f"{run.completed_tasks}/{run.total_tasks}"
            )
            runs_table.add_row(
                run.run_id,
                run.state,
                run.current_stage,
                progress,
                key=run.run_id,
            )

        selected_run_id = (
            self._selected_run_id
            if self._selected_run_id in self._runs_by_id
            else (snapshot.runs[0].run_id if snapshot.runs else None)
        )
        self._selected_run_id = selected_run_id
        if selected_run_id is None:
            self._selected_task_id = None
            self.query_one("#selected-run", Static).update("No factory run selected.")
            self.query_one("#tasks-table", DataTable).clear(columns=False)
            self.query_one("#task-details", Static).update("No task selected.")
            self.query_one("#activity-page", Static).update("Page 1/1")
            self.query_one("#activity-log", Static).update("No activity recorded.")
            self.query_one("#attention-list", Static).update(
                self._render_attention(snapshot.attention_items)
            )
            self.query_one("#outputs", Static).update("No outputs selected.")
            self.query_one("#review-summary", Static).update(
                "Review summary not available."
            )
            return

        row_index = next(
            index
            for index, run in enumerate(snapshot.runs)
            if run.run_id == selected_run_id
        )
        if snapshot.runs:
            runs_table.move_cursor(row=row_index, column=0)
        self._render_run(self._runs_by_id[selected_run_id])

    def on_data_table_row_highlighted(self, event: DataTable.RowHighlighted) -> None:
        run_id = event.row_key.value
        if event.data_table.id == "runs-table" and run_id in self._runs_by_id:
            self._selected_run_id = run_id
            self._render_run(self._runs_by_id[run_id])
        elif event.data_table.id == "tasks-table" and run_id is not None:
            self._render_task(run_id)

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        row_key = event.row_key.value
        if event.data_table.id == "runs-table" and row_key in self._runs_by_id:
            self._selected_run_id = row_key
            self._render_run(self._runs_by_id[row_key])
        elif event.data_table.id == "tasks-table" and row_key is not None:
            self._render_task(row_key)

    def _render_run(self, run: DashboardRun) -> None:
        self._selected_run_id = run.run_id
        progress = (
            f"{run.completed_tasks}/{run.total_tasks} tasks "
            f"({run.completion_percentage}% completed)"
            if run.completion_percentage is not None
            else f"{run.completed_tasks}/{run.total_tasks} tasks completed"
        )
        self.query_one("#selected-run", Static).update(
            f"{run.run_id} | {run.state} | stage: {run.current_stage} | {progress}"
        )
        tasks_table = self.query_one("#tasks-table", DataTable)
        tasks_table.clear(columns=False)
        for task in run.tasks:
            tasks_table.add_row(
                task.task_id,
                task.state.value if task.state is not None else "unknown",
                task.worker_id or "—",
                str(task.attempt_count),
                key=task.task_id,
            )
        self._current_tasks = {task.task_id: task for task in run.tasks}
        selected_task_id = (
            self._selected_task_id
            if self._selected_task_id in self._current_tasks
            else (run.tasks[0].task_id if run.tasks else None)
        )
        self._selected_task_id = selected_task_id
        if selected_task_id is not None:
            task_index = next(
                index
                for index, task in enumerate(run.tasks)
                if task.task_id == selected_task_id
            )
            tasks_table.move_cursor(row=task_index, column=0)
            self._render_task(selected_task_id)
        else:
            self.query_one("#task-details", Static).update("No tasks in this run.")

        self._activity_offset = 0
        self._render_activity_page(
            tuple(run.activity_events),
            offset=0,
            total_events=run.activity_total_events,
            page_count=run.activity_page_count,
        )
        self.query_one("#attention-list", Static).update(
            self._render_attention(run.attention_items)
        )
        self.query_one("#outputs", Static).update(self._render_outputs(run))
        review_summary = run.review_summary or "Review summary not available yet."
        if run.review_state is not None:
            review_summary = f"Review state: {run.review_state}\n{review_summary}"
        self.query_one("#review-summary", Static).update(review_summary)

    def _render_task(self, task_id: str) -> None:
        task = self._current_tasks.get(task_id)
        if task is None:
            return
        self._selected_task_id = task_id
        state = task.state.value if task.state is not None else "unknown"
        lines = [
            f"{task.task_id} | {state}",
            task.description,
            f"Worker: {task.worker_id or 'unassigned'} | "
            f"Attempts: {task.attempt_count}",
        ]
        if task.reason is not None:
            lines.append(f"Latest reason: {task.reason}")
        if task.attempts:
            lines.append("Attempt history:")
            lines.extend(
                f"  #{attempt.attempt_number} {attempt.status.value}"
                + (f" - {attempt.reason}" if attempt.reason else "")
                for attempt in task.attempts
            )
        self.query_one("#task-details", Static).update("\n".join(lines))

    def _render_activity_page(
        self,
        events: tuple[ActivityEvent, ...],
        *,
        offset: int,
        total_events: int,
        page_count: int,
    ) -> None:
        page_number = min(offset // self.read_model.events_per_run + 1, page_count)
        self._activity_offset = offset
        self._activity_total_events = total_events
        self.query_one("#activity-page", Static).update(
            f"Page {page_number}/{page_count} | {total_events} events | "
            "n older, p newer"
        )
        if not events:
            self.query_one("#activity-log", Static).update("No activity recorded.")
            return
        lines = [
            f"{event.occurred_at.astimezone().strftime('%H:%M:%S')} "
            f"[{event.category}] {event.summary}"
            for event in events
        ]
        self.query_one("#activity-log", Static).update("\n".join(lines))

    @staticmethod
    def _render_attention(items: tuple[DashboardAttention, ...]) -> str:
        if not items:
            return "No pending human decisions."
        lines = []
        for item in items:
            actions = ", ".join(action.value for action in item.available_actions)
            lines.append(f"{item.state}: {item.reason}")
            if item.latest_result:
                lines.append(f"Latest: {item.latest_result}")
            if actions:
                lines.append(f"Available through CLI: {actions}")
        return "\n".join(lines)

    @staticmethod
    def _render_outputs(run: DashboardRun) -> str:
        lines = []
        for label, path in (
            ("Worktree", run.worktree_path),
            ("OpenSpec change", run.open_spec_path),
        ):
            if path is None:
                lines.append(f"{label}: unavailable (no recorded path)")
            else:
                availability = "available" if path.exists() else "unavailable"
                lines.append(f"{label}: {availability} | {path}")
        return "\n".join(lines)

    @work(thread=True, exclusive=True, group="dashboard-activity")
    def _load_activity_page(self, run_id: str, offset: int) -> None:
        try:
            page = self.read_model.activity_page(run_id, offset=offset)
        except Exception as error:
            message = (
                sanitize_activity_summary(str(error)) or "Unable to load activity."
            )
            self.call_from_thread(self._show_refresh_error, message)
            return
        self.call_from_thread(self._apply_activity_page, run_id, page)

    def _apply_activity_page(self, run_id: str, page: ActivityPage) -> None:
        if run_id != self._selected_run_id:
            return
        self._render_activity_page(
            page.events,
            offset=page.offset,
            total_events=page.total_events,
            page_count=page.page_count,
        )

    def action_next_activity_page(self) -> None:
        run_id = self._selected_run_id
        if (
            run_id is not None
            and self._activity_offset + self.read_model.events_per_run
            < self._activity_total_events
        ):
            self._load_activity_page(
                run_id,
                self._activity_offset + self.read_model.events_per_run,
            )

    def action_previous_activity_page(self) -> None:
        run_id = self._selected_run_id
        if run_id is not None and self._activity_offset > 0:
            self._load_activity_page(
                run_id,
                max(0, self._activity_offset - self.read_model.events_per_run),
            )

    def action_refresh_data(self) -> None:
        self._refresh_snapshot()
