from pathlib import Path

from cronos_ai.controller import LocalController
from cronos_ai.models import WorkRequest
from cronos_ai.storage import FactoryStore


def test_controller_restart_reuses_persisted_queue_without_duplicates(
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "factory-state"
    database_path = state_dir / "factory.sqlite3"
    request = WorkRequest(
        request_id="request-stable-id",
        description="Continue queued work after restart",
        repo_path="/workspace/project",
    )
    with FactoryStore(database_path) as store:
        store.enqueue_request(request)

    for _ in range(2):
        with LocalController(state_dir):
            with FactoryStore(database_path) as store:
                assert store.list_queued_requests() == [request]

    with FactoryStore(database_path) as store:
        assert store.list_requests() == [request]
        assert len(store.list_queued_requests()) == 1
        status = store.get_controller_status()

    assert status is not None
    assert status.state.value == "stopped"
