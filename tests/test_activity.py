import sqlite3
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from cronos_ai.activity import sanitize_activity_summary
from cronos_ai.models import (
    ActivityEvent,
    Attempt,
    AttemptStatus,
    OpenSpecPlan,
    PlanTask,
    TaskRecord,
    TaskState,
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


def test_run_creation_is_persisted_as_activity_event(tmp_path: Path) -> None:
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store)
        events = store.list_activity_events("run-1")

    assert len(events) == 1
    assert events[0].category == "run-lifecycle"
    assert events[0].summary == "Factory run created"
    assert events[0].task_id is None


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
        assert upgraded.list_activity_events("run-1", task_id="task-1") == [event]


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
        stored = reopened.list_activity_events("run-1", task_id="task-1")[0]

    assert stored == event
    assert stored.event_id == "event-1"
    assert stored.occurred_at.utcoffset() == timedelta(0)


def test_sanitizer_redacts_common_provider_token_formats() -> None:
    secrets = (
        "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "Bearer eyJhbGciOiJIUzI1NiJ9.secret.signature",
        "AKIAIOSFODNN7EXAMPLE",
    )
    summary = "Credentials: " + "; ".join(secrets)

    sanitized = sanitize_activity_summary(summary)

    assert sanitized is not None
    assert all(secret not in sanitized for secret in secrets)
    assert "[REDACTED]" in sanitized


def test_sanitizer_redacts_secret_environment_values(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "provider-secret-value-123456789"
    monkeypatch.setenv("CRONOS_AI_TEST_API_KEY", secret)

    sanitized = sanitize_activity_summary(f"Worker failed using {secret}")

    assert sanitized is not None
    assert secret not in sanitized
    assert "[REDACTED]" in sanitized


def test_sanitizer_bounds_oversized_summaries() -> None:
    sanitized = sanitize_activity_summary("progress " * 100)

    assert sanitized is not None
    assert len(sanitized) <= 512
    assert sanitized.endswith("[truncated]")


def test_store_redacts_environment_secret_before_persisting_event(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "worker-provider-secret-123456"
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", secret)
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store)
        store.append_activity_event(
            _event("secret-event").model_copy(
                update={"summary": f"Provider rejected token {secret}"}
            )
        )
        events = store.list_activity_events("run-1", task_id="task-1")

    assert len(events) == 1
    assert secret not in events[0].summary
    assert "[REDACTED]" in events[0].summary


def test_task_state_transitions_are_persisted_once(tmp_path: Path) -> None:
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store)
        store.save_task(
            "run-1",
            TaskRecord(task_id="task-1", state=TaskState.READY),
        )
        store.save_task(
            "run-1",
            TaskRecord(task_id="task-1", state=TaskState.READY),
        )
        events = store.list_activity_events("run-1", task_id="task-1")

    assert [event.summary for event in events] == ["Task entered ready state"]


def test_attempt_lifecycle_events_keep_failure_summaries_safe(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "factory-provider-key-123456"
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", secret)
    started_at = datetime.now(UTC)
    running_attempt = Attempt(
        task_id="task-1",
        attempt_number=1,
        status=AttemptStatus.RUNNING,
        started_at=started_at,
        worker_id="worker-1",
    )
    failed_attempt = Attempt(
        task_id="task-1",
        attempt_number=1,
        status=AttemptStatus.FAILED,
        started_at=started_at,
        finished_at=started_at + timedelta(seconds=2),
        worker_id="worker-1",
        reason=f"Provider rejected credential {secret}",
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        _create_run(store)
        store.record_attempt(
            "run-1",
            TaskRecord(
                task_id="task-1",
                state=TaskState.RUNNING,
                attempt_count=1,
                worker_id="worker-1",
            ),
            running_attempt,
        )
        store.finish_attempt(
            "run-1",
            TaskRecord(
                task_id="task-1",
                state=TaskState.FAILED,
                reason="Provider rejected the attempt.",
                attempt_count=1,
            ),
            failed_attempt,
        )
        events = store.list_activity_events("run-1", task_id="task-1")

    attempt_events = [event for event in events if event.category == "attempt"]
    assert [event.summary for event in attempt_events] == [
        "Attempt 1 failed: Provider rejected credential [REDACTED]",
        "Attempt 1 running",
    ]
    assert secret not in str(events)


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

        newest_page = store.list_activity_events(
            "run-1", task_id="task-1", limit=1
        )
        older_page = store.list_activity_events(
            "run-1", task_id="task-1", limit=1, offset=1
        )
        run_two_events = store.list_activity_events("run-2")
        task_events = store.list_activity_events("run-1", task_id="task-1")

    assert newest_page == [events[1]]
    assert older_page == [events[0]]
    assert events[2] in run_two_events
    assert all(event.run_id == "run-2" for event in run_two_events)
    assert task_events == [events[1], events[0]]
