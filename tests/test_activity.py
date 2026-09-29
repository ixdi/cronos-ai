import sqlite3
from datetime import UTC, datetime, timedelta
from pathlib import Path

from cronos_ai.models import (
    ActivityEvent,
    OpenSpecPlan,
    PlanTask,
    TaskRecord,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore


def _create_run(store: FactoryStore, run_id: str = "run-1") -> None:
    store.create_run(
        run_id,
        WorkRequest(
            request_id=f"request-{run_id}",
            description="Inspect factory progress",
            repo_path=Path("/workspace/project"),
        ),
        OpenSpecPlan(
            change_name="monitor-progress",
            tasks=(PlanTask(task_id="task-1", description="Build dashboard"),),
        ),
        (TaskRecord(task_id="task-1"),),
    )


def _event(
    event_id: str,
    *,
    run_id: str = "run-1",
    task_id: str | None = "task-1",
    occurred_at: datetime | None = None,
) -> ActivityEvent:
    return ActivityEvent(
        event_id=event_id,
        run_id=run_id,
        task_id=task_id,
        occurred_at=occurred_at or datetime.now(UTC),
        category="progress",
        summary=f"Progress event {event_id}",
    )


def test_activity_migration_upgrades_previous_schema_and_preserves_runs(
    tmp_path: Path,
) -> None:
    database_path = tmp_path / "factory.sqlite3"
    with FactoryStore(database_path) as store:
        _create_run(store)

    connection = sqlite3.connect(database_path)
    try:
        connection.execute("DROP TABLE IF EXISTS factory_activity_events")
        connection.execute("PRAGMA user_version = 9")
        connection.commit()
    finally:
        connection.close()

    with FactoryStore(database_path) as upgraded:
        assert upgraded.schema_version == 10
        assert upgraded.get_run("run-1") is not None
        event = _event("event-after-upgrade")
        upgraded.append_activity_event(event)
        assert upgraded.list_activity_events("run-1") == [event]


def test_activity_event_round_trips_with_utc_timestamp_and_identity(
    tmp_path: Path,
) -> None:
    database_path = tmp_path / "factory.sqlite3"
    occurred_at = datetime(2026, 9, 29, 12, 30, tzinfo=UTC)
    event = _event("event-1", occurred_at=occurred_at)

    with FactoryStore(database_path) as store:
        _create_run(store)
        store.append_activity_event(event)

    with FactoryStore(database_path) as reopened:
        stored = reopened.list_activity_events("run-1")[0]

    assert stored == event
    assert stored.event_id == "event-1"
    assert stored.occurred_at.utcoffset() == timedelta(0)


def test_activity_events_filter_by_run_and_task_and_page_newest_first(
    tmp_path: Path,
) -> None:
    base_time = datetime(2026, 9, 29, 12, 30, tzinfo=UTC)
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store)
        _create_run(store, "run-2")
        events = (
            _event("event-1", occurred_at=base_time),
            _event("event-2", occurred_at=base_time + timedelta(seconds=1)),
            _event(
                "event-3",
                run_id="run-2",
                task_id=None,
                occurred_at=base_time + timedelta(seconds=2),
            ),
        )
        for event in events:
            store.append_activity_event(event)

        newest_page = store.list_activity_events("run-1", limit=1)
        older_page = store.list_activity_events("run-1", limit=1, offset=1)
        run_two_events = store.list_activity_events("run-2")
        task_events = store.list_activity_events("run-1", task_id="task-1")

    assert newest_page == [events[1]]
    assert older_page == [events[0]]
    assert run_two_events == [events[2]]
    assert task_events == [events[1], events[0]]
