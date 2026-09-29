"""SQLite persistence and process locking for the local factory controller."""

from __future__ import annotations

import fcntl
import os
import sqlite3
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from threading import RLock
from typing import cast

from cronos_ai.models import (
    ActivityEvent,
    Attempt,
    AttemptStatus,
    ControlAction,
    ControlActionRecord,
    ControlActionStatus,
    ControllerStatus,
    DeliveryRecord,
    OpenSpecPlan,
    PlanApproval,
    ReviewPacket,
    RunContext,
    TaskRecord,
    WebhookEvent,
    WebhookReceipt,
    WorkerSlot,
    WorkRequest,
)


class StorageError(RuntimeError):
    """Raised when the local execution ledger cannot be safely opened."""


class ControllerLockError(StorageError):
    """Raised when another controller already owns this factory's state."""


_MIGRATIONS: dict[int, tuple[str, ...]] = {
    1: (
        """
        CREATE TABLE runs (
            run_id TEXT PRIMARY KEY,
            request_json TEXT NOT NULL,
            plan_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        """,
        """
        CREATE TABLE tasks (
            run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            state TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (run_id, task_id),
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE
        )
        """,
        """
        CREATE TABLE attempts (
            run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
            status TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            PRIMARY KEY (run_id, task_id, attempt_number),
            FOREIGN KEY (run_id, task_id)
                REFERENCES tasks(run_id, task_id) ON DELETE CASCADE
        )
        """,
    ),
    2: (
        """
        CREATE TABLE plan_approvals (
            approval_id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            change_name TEXT NOT NULL,
            plan_hash TEXT NOT NULL,
            decision TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
            payload_json TEXT NOT NULL,
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE
        )
        """,
        """
        CREATE INDEX idx_plan_approvals_lookup
        ON plan_approvals(run_id, change_name, plan_hash, approval_id)
        """,
    ),
    3: (
        """
        CREATE TABLE requests (
            request_id TEXT PRIMARY KEY,
            status TEXT NOT NULL CHECK (
                status IN ('queued', 'processing', 'done', 'blocked')
            ),
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        """,
        """
        CREATE TABLE control_actions (
            action_id TEXT PRIMARY KEY,
            action_type TEXT NOT NULL,
            target_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK (
                status IN ('pending', 'processing', 'done', 'failed')
            ),
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        """,
        """
        CREATE TABLE controller_state (
            singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
            payload_json TEXT NOT NULL
        )
        """,
    ),
    4: (
        """
        CREATE TABLE worker_slots (
            worker_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            workspace_id TEXT,
            pane_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK (
                status IN ('idle', 'working', 'blocked', 'done')
            ),
            active_task_id TEXT,
            payload_json TEXT NOT NULL,
            UNIQUE (session_id, pane_id)
        )
        """,
        """
        CREATE INDEX idx_worker_slots_active_task
        ON worker_slots(active_task_id, status)
        """,
    ),
    5: (
        "ALTER TABLE tasks ADD COLUMN worker_id TEXT",
        """
        UPDATE tasks
        SET worker_id = json_extract(payload_json, '$.worker_id')
        WHERE json_type(payload_json, '$.worker_id') = 'text'
        """,
        """
        CREATE INDEX idx_tasks_active_worker
        ON tasks(state, worker_id)
        """,
    ),
    6: (
        "ALTER TABLE control_actions ADD COLUMN error_message TEXT",
        "ALTER TABLE control_actions ADD COLUMN processed_at TEXT",
        """
        CREATE TABLE run_contexts (
            run_id TEXT PRIMARY KEY,
            payload_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE
        )
        """,
    ),
    7: (
        """
        CREATE TABLE review_packets (
            review_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            state TEXT NOT NULL,
            packet_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE
        )
        """,
        "CREATE INDEX idx_review_packets_latest ON review_packets(run_id, created_at)",
    ),
    8: (
        """
        CREATE TABLE webhook_events (
            source_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            body_sha256 TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'received'
                CHECK (status IN ('received', 'processed', 'rejected')),
            event_json TEXT NOT NULL,
            received_at TEXT NOT NULL,
            PRIMARY KEY (source_id, event_id)
        )
        """,
        """
        CREATE INDEX idx_webhook_events_pending
        ON webhook_events(status, received_at)
        """,
    ),
    9: (
        """
        CREATE TABLE delivery_attempts (
            run_id TEXT NOT NULL,
            review_hash TEXT NOT NULL,
            status TEXT NOT NULL CHECK (
                status IN ('pending', 'running', 'succeeded', 'failed', 'inconclusive')
            ),
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (run_id, review_hash),
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE
        )
        """,
        """
        CREATE INDEX idx_delivery_attempts_latest
        ON delivery_attempts(run_id, updated_at)
        """,
    ),
    10: (
        """
        CREATE TABLE factory_activity_events (
            event_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            task_id TEXT,
            occurred_at TEXT NOT NULL,
            category TEXT NOT NULL,
            summary TEXT NOT NULL,
            FOREIGN KEY (run_id) REFERENCES runs(run_id) ON DELETE CASCADE,
            FOREIGN KEY (run_id, task_id)
                REFERENCES tasks(run_id, task_id) ON DELETE CASCADE
        )
        """,
        """
        CREATE INDEX idx_factory_activity_events_run_order
        ON factory_activity_events(run_id, occurred_at DESC, event_id DESC)
        """,
        """
        CREATE INDEX idx_factory_activity_events_task_order
        ON factory_activity_events(run_id, task_id, occurred_at DESC, event_id DESC)
        """,
    ),
}


class FactoryStore:
    """Versioned SQLite ledger for runs, tasks, and task attempts."""

    def __init__(self, database_path: Path) -> None:
        self.database_path = database_path
        self.database_path.parent.mkdir(parents=True, exist_ok=True)
        self._connection = sqlite3.connect(
            database_path,
            isolation_level=None,
            check_same_thread=False,
        )
        self._connection.row_factory = sqlite3.Row
        self._connection.execute("PRAGMA foreign_keys = ON")
        self._connection.execute("PRAGMA busy_timeout = 5000")
        self._connection.execute("PRAGMA journal_mode = WAL")
        self._mutex = RLock()
        self._lock_fd: int | None = None
        self._closed = False
        self._migrate()

    @property
    def schema_version(self) -> int:
        """Return the schema version recorded by SQLite."""
        with self._mutex:
            self._ensure_open()
            row = self._connection.execute("PRAGMA user_version").fetchone()
            return int(row[0])

    def _migrate(self) -> None:
        current_version = self.schema_version
        latest_version = max(_MIGRATIONS, default=0)
        if current_version > latest_version:
            raise StorageError(
                f"database schema version {current_version} is newer than "
                f"supported version {latest_version}"
            )

        for version in range(current_version + 1, latest_version + 1):
            statements = _MIGRATIONS.get(version)
            if statements is None:
                raise StorageError(f"database migration {version} is unavailable")
            self._connection.execute("BEGIN EXCLUSIVE")
            try:
                for statement in statements:
                    self._connection.execute(statement)
                self._connection.execute(f"PRAGMA user_version = {version}")
                self._connection.commit()
            except BaseException:
                self._connection.rollback()
                raise

    @contextmanager
    def _transaction(self) -> Iterator[sqlite3.Connection]:
        with self._mutex:
            self._ensure_open()
            self._connection.execute("BEGIN IMMEDIATE")
            try:
                yield self._connection
            except BaseException:
                self._connection.rollback()
                raise
            else:
                self._connection.commit()

    def _ensure_open(self) -> None:
        if self._closed:
            raise StorageError("factory store is closed")

    def _fetchone(
        self,
        query: str,
        parameters: tuple[object, ...] = (),
    ) -> sqlite3.Row | None:
        with self._mutex:
            self._ensure_open()
            return cast(
                sqlite3.Row | None,
                self._connection.execute(query, parameters).fetchone(),
            )

    def _fetchall(
        self,
        query: str,
        parameters: tuple[object, ...] = (),
    ) -> list[sqlite3.Row]:
        with self._mutex:
            self._ensure_open()
            return cast(
                list[sqlite3.Row],
                self._connection.execute(query, parameters).fetchall(),
            )

    def create_run(
        self,
        run_id: str,
        request: WorkRequest,
        plan: OpenSpecPlan,
        task_records: tuple[TaskRecord, ...] = (),
    ) -> None:
        """Persist a run, its plan, and initial task states atomically."""
        if not run_id.strip():
            raise ValueError("run identifier must not be empty")
        created_at = datetime.now(UTC).isoformat()
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO runs (run_id, request_json, plan_json, created_at)
                VALUES (?, ?, ?, ?)
                """,
                (run_id, request.model_dump_json(), plan.model_dump_json(), created_at),
            )
            if task_records:
                expected_ids = {task.task_id for task in plan.tasks}
                actual_ids = {task.task_id for task in task_records}
                if expected_ids != actual_ids or len(actual_ids) != len(task_records):
                    raise ValueError(
                        "initial task records must match the OpenSpec plan"
                    )
                for task in task_records:
                    self._write_task(connection, run_id, task)

    def get_run(self, run_id: str) -> tuple[WorkRequest, OpenSpecPlan] | None:
        """Load a run's request and plan, if it exists."""
        row = self._fetchone(
            "SELECT request_json, plan_json FROM runs WHERE run_id = ?",
            (run_id,),
        )
        if row is None:
            return None
        return (
            WorkRequest.model_validate_json(row["request_json"]),
            OpenSpecPlan.model_validate_json(row["plan_json"]),
        )

    def list_run_ids(self) -> list[str]:
        """Return persisted run identifiers in creation order."""
        rows = self._fetchall("SELECT run_id FROM runs ORDER BY created_at, run_id")
        return [str(row["run_id"]) for row in rows]

    def save_run_context(self, context: RunContext) -> None:
        """Persist durable paths and approval identity for a run."""
        with self._transaction() as connection:
            if (
                connection.execute(
                    "SELECT 1 FROM runs WHERE run_id = ?", (context.run_id,)
                ).fetchone()
                is None
            ):
                raise StorageError(f"run does not exist: {context.run_id}")
            connection.execute(
                """
                INSERT INTO run_contexts (run_id, payload_json, updated_at)
                VALUES (?, ?, ?)
                ON CONFLICT (run_id) DO UPDATE SET
                    payload_json = excluded.payload_json,
                    updated_at = excluded.updated_at
                """,
                (
                    context.run_id,
                    context.model_dump_json(),
                    datetime.now(UTC).isoformat(),
                ),
            )

    def get_run_context(self, run_id: str) -> RunContext | None:
        """Load a run's persisted branch, worktree, and plan references."""
        row = self._fetchone(
            "SELECT payload_json FROM run_contexts WHERE run_id = ?", (run_id,)
        )
        if row is None:
            return None
        return RunContext.model_validate_json(row["payload_json"])

    def save_task(self, run_id: str, task: TaskRecord) -> None:
        """Insert or update a task's durable current state."""
        with self._transaction() as connection:
            self._write_task(connection, run_id, task)

    def _write_task(
        self,
        connection: sqlite3.Connection,
        run_id: str,
        task: TaskRecord,
    ) -> None:
        connection.execute(
            """
            INSERT INTO tasks (
                run_id, task_id, state, worker_id, payload_json, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT (run_id, task_id) DO UPDATE SET
                state = excluded.state,
                worker_id = excluded.worker_id,
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            """,
            (
                run_id,
                task.task_id,
                task.state.value,
                task.worker_id,
                task.model_dump_json(),
                datetime.now(UTC).isoformat(),
            ),
        )

    def save_tasks(self, run_id: str, tasks: tuple[TaskRecord, ...]) -> None:
        """Atomically persist a batch of task lifecycle transitions."""
        with self._transaction() as connection:
            for task in tasks:
                self._write_task(connection, run_id, task)

    def list_tasks(self, run_id: str) -> list[TaskRecord]:
        """Load a run's current task records."""
        rows = self._fetchall(
            """
            SELECT payload_json FROM tasks
            WHERE run_id = ?
            ORDER BY task_id
            """,
            (run_id,),
        )
        return [TaskRecord.model_validate_json(row["payload_json"]) for row in rows]

    def get_task(self, run_id: str, task_id: str) -> TaskRecord | None:
        """Load a task's latest durable state, if it exists."""
        row = self._fetchone(
            "SELECT payload_json FROM tasks WHERE run_id = ? AND task_id = ?",
            (run_id, task_id),
        )
        if row is None:
            return None
        return TaskRecord.model_validate_json(row["payload_json"])

    def record_attempt(
        self,
        run_id: str,
        task: TaskRecord,
        attempt: Attempt,
    ) -> None:
        """Atomically update task state and append one immutable attempt."""
        if task.task_id != attempt.task_id:
            raise ValueError("attempt task identifier does not match task record")
        with self._transaction() as connection:
            self._write_task(connection, run_id, task)
            connection.execute(
                """
                INSERT INTO attempts (
                    run_id, task_id, attempt_number, status, payload_json
                ) VALUES (?, ?, ?, ?, ?)
                """,
                (
                    run_id,
                    attempt.task_id,
                    attempt.attempt_number,
                    attempt.status.value,
                    attempt.model_dump_json(),
                ),
            )

    def finish_attempt(
        self,
        run_id: str,
        task: TaskRecord,
        attempt: Attempt,
        *,
        worker_slot_update: WorkerSlot | None = None,
    ) -> None:
        """Atomically persist a terminal attempt outcome and task transition."""
        if task.task_id != attempt.task_id:
            raise ValueError("attempt task identifier does not match task record")
        if attempt.status is AttemptStatus.RUNNING:
            raise ValueError("finished attempt status must be terminal")
        if task.attempt_count < attempt.attempt_number:
            raise ValueError("task attempt count cannot predate the attempt")
        with self._transaction() as connection:
            self._write_task(connection, run_id, task)
            updated = connection.execute(
                """
                UPDATE attempts SET status = ?, payload_json = ?
                WHERE run_id = ? AND task_id = ? AND attempt_number = ?
                    AND status = 'running'
                """,
                (
                    attempt.status.value,
                    attempt.model_dump_json(),
                    run_id,
                    attempt.task_id,
                    attempt.attempt_number,
                ),
            )
            if updated.rowcount != 1:
                raise StorageError(
                    "running attempt not found for terminal state transition"
                )
            if worker_slot_update is not None:
                self._write_worker_slot(connection, worker_slot_update)

    def get_attempts(self, run_id: str, task_id: str) -> list[Attempt]:
        """Load a task's attempts in attempt-number order."""
        rows = self._fetchall(
            """
            SELECT payload_json FROM attempts
            WHERE run_id = ? AND task_id = ?
            ORDER BY attempt_number
            """,
            (run_id, task_id),
        )
        return [Attempt.model_validate_json(row["payload_json"]) for row in rows]

    def append_activity_event(self, event: ActivityEvent) -> None:
        """Append one sanitized run or task activity event."""
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO factory_activity_events (
                    event_id, run_id, task_id, occurred_at, category, summary
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    event.event_id,
                    event.run_id,
                    event.task_id,
                    event.occurred_at.isoformat(),
                    event.category,
                    event.summary,
                ),
            )

    def list_activity_events(
        self,
        run_id: str,
        *,
        task_id: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[ActivityEvent]:
        """Load a bounded, newest-first page of activity for one run or task."""
        if limit < 1 or limit > 1000:
            raise ValueError("activity event limit must be between 1 and 1000")
        if offset < 0:
            raise ValueError("activity event offset must not be negative")
        task_filter = " AND task_id = ?" if task_id is not None else ""
        parameters: tuple[object, ...] = (run_id,)
        if task_id is not None:
            parameters += (task_id,)
        parameters += (limit, offset)
        rows = self._fetchall(
            f"""
            SELECT event_id, run_id, task_id, occurred_at, category, summary
            FROM factory_activity_events
            WHERE run_id = ?{task_filter}
            ORDER BY occurred_at DESC, event_id DESC
            LIMIT ? OFFSET ?
            """,
            parameters,
        )
        return [
            ActivityEvent(
                event_id=str(row["event_id"]),
                run_id=str(row["run_id"]),
                task_id=(str(row["task_id"]) if row["task_id"] is not None else None),
                occurred_at=datetime.fromisoformat(str(row["occurred_at"])),
                category=str(row["category"]),
                summary=str(row["summary"]),
            )
            for row in rows
        ]

    def record_plan_approval(self, run_id: str, approval: PlanApproval) -> None:
        """Append a plan approval or rejection to the durable audit history."""
        with self._transaction() as connection:
            row = connection.execute(
                "SELECT plan_json FROM runs WHERE run_id = ?",
                (run_id,),
            ).fetchone()
            if row is None:
                raise StorageError(f"run does not exist: {run_id}")
            run_plan = OpenSpecPlan.model_validate_json(row["plan_json"])
            if run_plan.change_name != approval.change_name:
                raise ValueError("approval change does not match the run plan")
            context_row = connection.execute(
                "SELECT payload_json FROM run_contexts WHERE run_id = ?",
                (run_id,),
            ).fetchone()
            if context_row is not None:
                context = RunContext.model_validate_json(context_row["payload_json"])
                if (
                    context.plan_hash is not None
                    and context.plan_hash != approval.plan_hash
                ):
                    raise ValueError(
                        "approval fingerprint does not match the stored run plan"
                    )
            connection.execute(
                """
                INSERT INTO plan_approvals (
                    run_id, change_name, plan_hash, decision, payload_json
                ) VALUES (?, ?, ?, ?, ?)
                """,
                (
                    run_id,
                    approval.change_name,
                    approval.plan_hash,
                    approval.decision.value,
                    approval.model_dump_json(),
                ),
            )

    def get_plan_approval(
        self,
        run_id: str,
        change_name: str,
        plan_hash: str,
    ) -> PlanApproval | None:
        """Return the latest decision for this exact plan version."""
        row = self._fetchone(
            """
            SELECT payload_json FROM plan_approvals
            WHERE run_id = ? AND change_name = ? AND plan_hash = ?
            ORDER BY approval_id DESC LIMIT 1
            """,
            (run_id, change_name, plan_hash),
        )
        if row is None:
            return None
        return PlanApproval.model_validate_json(row["payload_json"])

    def create_review_packet(self, packet: ReviewPacket) -> None:
        """Persist immutable review evidence for a run."""
        now = datetime.now(UTC).isoformat()
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO review_packets (
                    review_id, run_id, state, packet_json, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    packet.review_id,
                    packet.run_id,
                    packet.state.value,
                    packet.model_dump_json(),
                    packet.created_at.isoformat(),
                    now,
                ),
            )

    def create_blocked_review_packet(
        self,
        packet: ReviewPacket,
        tasks: tuple[TaskRecord, ...],
    ) -> None:
        """Persist failed review evidence and remediation transitions atomically."""
        if packet.state.value != "blocked":
            raise ValueError("only blocked review packets can be recorded this way")
        now = datetime.now(UTC).isoformat()
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO review_packets (
                    review_id, run_id, state, packet_json, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    packet.review_id,
                    packet.run_id,
                    packet.state.value,
                    packet.model_dump_json(),
                    packet.created_at.isoformat(),
                    now,
                ),
            )
            for task in tasks:
                self._write_task(connection, packet.run_id, task)

    def get_latest_review_packet(self, run_id: str) -> ReviewPacket | None:
        """Load the newest review packet for a run."""
        row = self._fetchone(
            """
            SELECT packet_json FROM review_packets
            WHERE run_id = ? ORDER BY created_at DESC, review_id DESC LIMIT 1
            """,
            (run_id,),
        )
        if row is None:
            return None
        return ReviewPacket.model_validate_json(row["packet_json"])

    def update_review_packet(self, packet: ReviewPacket) -> None:
        """Update the decision fields of a persisted review packet."""
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE review_packets
                SET state = ?, packet_json = ?, updated_at = ?
                WHERE review_id = ? AND run_id = ?
                """,
                (
                    packet.state.value,
                    packet.model_dump_json(),
                    datetime.now(UTC).isoformat(),
                    packet.review_id,
                    packet.run_id,
                ),
            )
            if cursor.rowcount != 1:
                raise StorageError(f"review packet does not exist: {packet.review_id}")

    def record_review_decision(
        self,
        packet: ReviewPacket,
        tasks: tuple[TaskRecord, ...],
    ) -> None:
        """Atomically record a human review decision and its task transitions."""
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE review_packets
                SET state = ?, packet_json = ?, updated_at = ?
                WHERE review_id = ? AND run_id = ? AND state = 'ready'
                """,
                (
                    packet.state.value,
                    packet.model_dump_json(),
                    datetime.now(UTC).isoformat(),
                    packet.review_id,
                    packet.run_id,
                ),
            )
            if cursor.rowcount != 1:
                raise StorageError("review packet is no longer awaiting a decision")
            for task in tasks:
                self._write_task(connection, packet.run_id, task)

    def record_webhook_event(self, event: WebhookEvent) -> WebhookReceipt:
        """Deduplicate authenticated webhook events by source and event ID."""
        with self._transaction() as connection:
            row = connection.execute(
                """
                SELECT body_sha256 FROM webhook_events
                WHERE source_id = ? AND event_id = ?
                """,
                (event.source_id, event.event_id),
            ).fetchone()
            if row is not None:
                if row["body_sha256"] == event.body_sha256:
                    return WebhookReceipt.DUPLICATE
                return WebhookReceipt.CONFLICT
            connection.execute(
                """
                INSERT INTO webhook_events (
                    source_id, event_id, body_sha256, status, event_json, received_at
                ) VALUES (?, ?, ?, 'received', ?, ?)
                """,
                (
                    event.source_id,
                    event.event_id,
                    event.body_sha256,
                    event.model_dump_json(),
                    event.received_at.isoformat(),
                ),
            )
        return WebhookReceipt.ACCEPTED

    def list_webhook_events(self) -> list[WebhookEvent]:
        """Return received webhook records in durable insertion order."""
        rows = self._fetchall(
            """
            SELECT event_json FROM webhook_events
            ORDER BY received_at, source_id, event_id
            """
        )
        return [WebhookEvent.model_validate_json(row["event_json"]) for row in rows]

    def list_received_webhook_events(self) -> list[WebhookEvent]:
        """Load authenticated events that ordinary intake has not consumed."""
        rows = self._fetchall(
            """
            SELECT event_json FROM webhook_events
            WHERE status = 'received' ORDER BY received_at, source_id, event_id
            """
        )
        return [WebhookEvent.model_validate_json(row["event_json"]) for row in rows]

    def mark_webhook_event_processed(
        self, source_id: str, event_id: str, *, rejected: bool = False
    ) -> None:
        """Mark one authenticated event consumed or rejected by ordinary triage."""
        status = "rejected" if rejected else "processed"
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE webhook_events SET status = ?
                WHERE source_id = ? AND event_id = ? AND status = 'received'
                """,
                (status, source_id, event_id),
            )
            if cursor.rowcount != 1:
                raise StorageError("received webhook event does not exist")

    def create_delivery_record(self, record: DeliveryRecord) -> None:
        """Persist a delivery attempt before dispatching external CI/CD work."""
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO delivery_attempts (
                    run_id, review_hash, status, payload_json, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    record.request.run_id,
                    record.request.review_hash,
                    record.status.value,
                    record.model_dump_json(),
                    record.created_at.isoformat(),
                    record.updated_at.isoformat(),
                ),
            )

    def update_delivery_record(self, record: DeliveryRecord) -> None:
        """Persist a CI/CD adapter result for an existing review-bound attempt."""
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE delivery_attempts
                SET status = ?, payload_json = ?, updated_at = ?
                WHERE run_id = ? AND review_hash = ?
                """,
                (
                    record.status.value,
                    record.model_dump_json(),
                    record.updated_at.isoformat(),
                    record.request.run_id,
                    record.request.review_hash,
                ),
            )
            if cursor.rowcount != 1:
                raise StorageError("delivery attempt does not exist")

    def get_delivery_record(
        self,
        run_id: str,
        review_hash: str | None = None,
    ) -> DeliveryRecord | None:
        """Load the latest delivery attempt, optionally for one review version."""
        if review_hash is None:
            row = self._fetchone(
                """
                SELECT payload_json FROM delivery_attempts
                WHERE run_id = ? ORDER BY created_at DESC LIMIT 1
                """,
                (run_id,),
            )
        else:
            row = self._fetchone(
                """
                SELECT payload_json FROM delivery_attempts
                WHERE run_id = ? AND review_hash = ?
                """,
                (run_id, review_hash),
            )
        if row is None:
            return None
        return DeliveryRecord.model_validate_json(row["payload_json"])

    def enqueue_request(self, request: WorkRequest) -> None:
        """Persist an intake request for the controller to process."""
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO requests (request_id, status, payload_json, created_at)
                VALUES (?, 'queued', ?, ?)
                """,
                (
                    request.request_id,
                    request.model_dump_json(),
                    datetime.now(UTC).isoformat(),
                ),
            )

    def list_requests(self) -> list[WorkRequest]:
        """Load queued and previously submitted work requests."""
        rows = self._fetchall(
            "SELECT payload_json FROM requests ORDER BY created_at, request_id"
        )
        return [WorkRequest.model_validate_json(row["payload_json"]) for row in rows]

    def enqueue_request_once(self, request: WorkRequest) -> bool:
        """Idempotently insert a webhook request without resetting its triage state."""
        with self._transaction() as connection:
            row = connection.execute(
                "SELECT payload_json FROM requests WHERE request_id = ?",
                (request.request_id,),
            ).fetchone()
            if row is not None:
                existing = WorkRequest.model_validate_json(row["payload_json"])
                if (
                    existing.source is not request.source
                    or existing.repo_path != request.repo_path
                    or existing.description != request.description
                ):
                    raise StorageError(
                        "request ID is already bound to different content"
                    )
                return False
            connection.execute(
                """
                INSERT INTO requests (request_id, status, payload_json, created_at)
                VALUES (?, 'queued', ?, ?)
                """,
                (
                    request.request_id,
                    request.model_dump_json(),
                    datetime.now(UTC).isoformat(),
                ),
            )
        return True

    def update_request(self, request: WorkRequest) -> None:
        """Update a request after clarification and make it eligible for triage."""
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE requests SET status = 'queued', payload_json = ?
                WHERE request_id = ?
                """,
                (request.model_dump_json(), request.request_id),
            )
            if cursor.rowcount != 1:
                raise StorageError(f"request does not exist: {request.request_id}")

    def list_queued_requests(self) -> list[WorkRequest]:
        """Load requests waiting for controller processing."""
        rows = self._fetchall(
            """
            SELECT payload_json FROM requests
            WHERE status = 'queued'
            ORDER BY created_at, request_id
            """
        )
        return [WorkRequest.model_validate_json(row["payload_json"]) for row in rows]

    def enqueue_control_action(self, action: ControlAction) -> None:
        """Persist a human action for controller processing."""
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO control_actions (
                    action_id, action_type, target_id, status, payload_json, created_at
                ) VALUES (?, ?, ?, 'pending', ?, ?)
                """,
                (
                    action.action_id,
                    action.action_type.value,
                    action.target_id,
                    action.model_dump_json(),
                    action.created_at.isoformat(),
                ),
            )

    def list_pending_actions(self) -> list[ControlAction]:
        """Load human actions that have not yet been handled by the controller."""
        rows = self._fetchall(
            """
            SELECT payload_json FROM control_actions
            WHERE status = 'pending'
            ORDER BY created_at, action_id
            """
        )
        return [ControlAction.model_validate_json(row["payload_json"]) for row in rows]

    def get_control_action_record(self, action_id: str) -> ControlActionRecord | None:
        """Load one action and its auditable processing outcome."""
        row = self._fetchone(
            """
            SELECT payload_json, status, error_message, processed_at
            FROM control_actions WHERE action_id = ?
            """,
            (action_id,),
        )
        if row is None:
            return None
        return ControlActionRecord(
            action=ControlAction.model_validate_json(row["payload_json"]),
            status=ControlActionStatus(row["status"]),
            error=row["error_message"],
            processed_at=(
                datetime.fromisoformat(row["processed_at"])
                if row["processed_at"] is not None
                else None
            ),
        )

    def list_control_action_records(
        self, status: ControlActionStatus | None = None
    ) -> list[ControlActionRecord]:
        """List action audit records, optionally filtered by lifecycle status."""
        if status is None:
            rows = self._fetchall(
                "SELECT action_id FROM control_actions ORDER BY created_at, action_id"
            )
        else:
            rows = self._fetchall(
                """
                SELECT action_id FROM control_actions
                WHERE status = ? ORDER BY created_at, action_id
                """,
                (status.value,),
            )
        records = [
            self.get_control_action_record(str(row["action_id"])) for row in rows
        ]
        return [record for record in records if record is not None]

    def requeue_interrupted_actions(self) -> int:
        """Return actions left processing by a stopped controller to pending."""
        with self._transaction() as connection:
            cursor = connection.execute(
                """
                UPDATE control_actions
                SET status = 'pending', error_message = NULL, processed_at = NULL
                WHERE status = 'processing'
                """
            )
            return cursor.rowcount

    def set_control_action_status(
        self,
        action_id: str,
        status: ControlActionStatus,
        error: str | None = None,
    ) -> ControlActionRecord:
        """Advance an action's auditable processing state."""
        with self._transaction() as connection:
            row = connection.execute(
                "SELECT status FROM control_actions WHERE action_id = ?",
                (action_id,),
            ).fetchone()
            if row is None:
                raise StorageError(f"control action does not exist: {action_id}")
            current = ControlActionStatus(row["status"])
            allowed = {
                ControlActionStatus.PENDING: {
                    ControlActionStatus.PROCESSING,
                    ControlActionStatus.FAILED,
                },
                ControlActionStatus.PROCESSING: {
                    ControlActionStatus.DONE,
                    ControlActionStatus.FAILED,
                },
            }
            if status not in allowed.get(current, set()):
                raise StorageError(
                    f"invalid action transition: {current.value} -> {status.value}"
                )
            if status is ControlActionStatus.FAILED and not error:
                raise ValueError("failed control actions require a reason")
            processed_at = (
                datetime.now(UTC).isoformat()
                if status in (ControlActionStatus.DONE, ControlActionStatus.FAILED)
                else None
            )
            connection.execute(
                """
                UPDATE control_actions
                SET status = ?, error_message = ?, processed_at = ?
                WHERE action_id = ?
                """,
                (status.value, error, processed_at, action_id),
            )
        record = self.get_control_action_record(action_id)
        if record is None:
            raise StorageError(f"control action disappeared: {action_id}")
        return record

    def set_controller_status(self, status: ControllerStatus) -> None:
        """Persist the controller's current lifecycle and heartbeat."""
        with self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO controller_state (singleton_id, payload_json)
                VALUES (1, ?)
                ON CONFLICT (singleton_id) DO UPDATE SET
                    payload_json = excluded.payload_json
                """,
                (status.model_dump_json(),),
            )

    def get_controller_status(self) -> ControllerStatus | None:
        """Return persisted controller health, if it has started before."""
        row = self._fetchone(
            "SELECT payload_json FROM controller_state WHERE singleton_id = 1"
        )
        if row is None:
            return None
        return ControllerStatus.model_validate_json(row["payload_json"])

    def save_worker_slot(self, slot: WorkerSlot) -> None:
        """Persist a reconnectable Herdr worker-to-pane association."""
        with self._transaction() as connection:
            self._write_worker_slot(connection, slot)

    @staticmethod
    def _write_worker_slot(
        connection: sqlite3.Connection,
        slot: WorkerSlot,
    ) -> None:
        connection.execute(
            """
            INSERT INTO worker_slots (
                worker_id, session_id, workspace_id, pane_id,
                status, active_task_id, payload_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (worker_id) DO UPDATE SET
                session_id = excluded.session_id,
                workspace_id = excluded.workspace_id,
                pane_id = excluded.pane_id,
                status = excluded.status,
                active_task_id = excluded.active_task_id,
                payload_json = excluded.payload_json
            """,
            (
                slot.worker_id,
                slot.session_id,
                slot.workspace_id,
                slot.pane_id,
                slot.status.value,
                slot.active_task_id,
                slot.model_dump_json(),
            ),
        )

    def get_worker_slot(self, worker_id: str) -> WorkerSlot | None:
        """Load a persisted worker slot by its stable factory identifier."""
        row = self._fetchone(
            "SELECT payload_json FROM worker_slots WHERE worker_id = ?",
            (worker_id,),
        )
        if row is None:
            return None
        return WorkerSlot.model_validate_json(row["payload_json"])

    def list_worker_slots(self, session_id: str) -> list[WorkerSlot]:
        """Load all persisted worker associations for a Herdr session."""
        rows = self._fetchall(
            """
            SELECT payload_json FROM worker_slots
            WHERE session_id = ?
            ORDER BY worker_id
            """,
            (session_id,),
        )
        return [WorkerSlot.model_validate_json(row["payload_json"]) for row in rows]

    def active_worker_ids(self) -> set[str]:
        """Return worker slots currently assigned to running or blocked tasks."""
        rows = self._fetchall(
            """
            SELECT worker_id FROM tasks
            WHERE state = 'running' AND worker_id IS NOT NULL
            UNION
            SELECT worker_id FROM worker_slots
            WHERE status IN ('working', 'blocked') AND active_task_id IS NOT NULL
            """
        )
        return {str(row["worker_id"]) for row in rows}

    def has_active_worker_for_task(
        self,
        task_id: str,
        *,
        excluding_worker_id: str | None = None,
    ) -> bool:
        """Check that a task is not already assigned to another live slot."""
        query = """
            SELECT 1 FROM worker_slots
            WHERE active_task_id = ? AND status IN ('working', 'blocked')
        """
        parameters: tuple[str, ...] = (task_id,)
        if excluding_worker_id is not None:
            query += " AND worker_id != ?"
            parameters += (excluding_worker_id,)
        return self._fetchone(query, parameters) is not None

    def delete_worker_slot(self, worker_id: str) -> None:
        """Remove an association only when its Herdr pane is no longer owned."""
        with self._transaction() as connection:
            connection.execute(
                "DELETE FROM worker_slots WHERE worker_id = ?",
                (worker_id,),
            )

    def acquire_controller_lock(self) -> None:
        """Acquire an exclusive process lock, released automatically on exit."""
        self._ensure_open()
        with self._mutex:
            if self._lock_fd is not None:
                return
            lock_path = self.database_path.with_suffix(
                f"{self.database_path.suffix}.lock"
            )
            file_descriptor = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
            try:
                fcntl.flock(file_descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                os.close(file_descriptor)
                raise ControllerLockError(
                    "another factory controller already holds the state lock"
                ) from error
            os.ftruncate(file_descriptor, 0)
            os.write(file_descriptor, str(os.getpid()).encode())
            self._lock_fd = file_descriptor

    def close(self) -> None:
        """Release the controller lock and close the database connection."""
        with self._mutex:
            if self._closed:
                return
            if self._lock_fd is not None:
                fcntl.flock(self._lock_fd, fcntl.LOCK_UN)
                os.close(self._lock_fd)
                self._lock_fd = None
            self._connection.close()
            self._closed = True

    def __enter__(self) -> FactoryStore:
        self._ensure_open()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
