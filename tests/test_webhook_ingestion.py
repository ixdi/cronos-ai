import hashlib
import hmac
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from uuid import NAMESPACE_URL, uuid5

from pydantic import SecretStr

from cronos_ai.approval import can_dispatch_implementation
from cronos_ai.intake import intake_request
from cronos_ai.models import (
    PlanRisk,
    RequestSource,
    TriageOutcome,
    TriageResult,
    WebhookAlertPayload,
)
from cronos_ai.storage import FactoryStore
from cronos_ai.triage import triage_request
from cronos_ai.webhook import WebhookEndpoint, WebhookSource
from cronos_ai.webhook_ingestion import WebhookIngestor

WEBHOOK_SECRET = "ingestion-test-secret-value-longer-than-thirty-two-bytes"


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def commit(repository: Path, message: str) -> None:
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "user.name=webhook-test",
            "-c",
            "user.email=webhook-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def create_repository(path: Path, *, openspec: bool = True) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(path, "config", "user.name", "webhook-test")
    git(path, "config", "user.email", "webhook-test@example.invalid")
    (path / "README.md").write_text("base\n")
    if openspec:
        (path / "openspec").mkdir()
        (path / "openspec" / "config.yaml").write_text("schema: spec-driven\n")
    git(path, "add", ".")
    commit(path, "initialize repository")
    return path


def accepted_alert(
    store: FactoryStore,
    repository: Path,
    *,
    event_id: str = "monitor-alert-1",
    description: str = "A production alert needs investigation",
) -> None:
    source = WebhookSource(
        source_id="monitor",
        secret=SecretStr(WEBHOOK_SECRET),
        allowed_repositories=(repository,),
    )
    endpoint = WebhookEndpoint(store, sources=(source,))
    payload = WebhookAlertPayload(repository=str(repository), description=description)
    body = payload.model_dump_json().encode("utf-8")
    timestamp = str(int(datetime.now(UTC).timestamp()))
    signature = hmac.new(
        WEBHOOK_SECRET.encode("utf-8"),
        timestamp.encode("ascii")
        + b".monitor."
        + event_id.encode("utf-8")
        + b"."
        + body,
        hashlib.sha256,
    ).hexdigest()
    response = endpoint.handle(
        "monitor",
        {
            "x-factory-timestamp": timestamp,
            "x-factory-event-id": event_id,
            "x-factory-signature": f"sha256={signature}",
        },
        body,
    )
    assert response.status_code == 202


def test_alert_becomes_untrusted_monitoring_request_for_ordinary_triage(
    tmp_path: Path,
) -> None:
    repository = create_repository(tmp_path / "repository")
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        accepted_alert(store, repository)
        source = WebhookSource(
            source_id="monitor",
            secret=SecretStr(WEBHOOK_SECRET),
            allowed_repositories=(repository,),
        )
        results = WebhookIngestor(store, sources=(source,)).process_pending()
        requests = store.list_queued_requests()

        assert len(results) == 1
        assert results[0].accepted
        assert len(requests) == 1
        request = requests[0]
        assert request.source is RequestSource.MONITORING
        assert request.triage_outcome is None
        assert request.repo_path == repository
        assert request.description == "A production alert needs investigation"

        triage: TriageResult = triage_request(
            request,
            TriageOutcome.ACTIONABLE,
            rationale="The alert is actionable but security-sensitive.",
            risks=(PlanRisk.SECURITY,),
        )
        assert triage.requires_approval
        assert not can_dispatch_implementation(
            triage,
            change_name="security-fix",
            plan_hash="a" * 64,
            approval=None,
        )
        assert store.list_run_ids() == []
        assert store.list_received_webhook_events() == []


def test_invalid_repository_is_rejected_by_normal_intake(tmp_path: Path) -> None:
    repository = create_repository(tmp_path / "without-openspec", openspec=False)
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        accepted_alert(store, repository)
        source = WebhookSource(
            source_id="monitor",
            secret=SecretStr(WEBHOOK_SECRET),
            allowed_repositories=(repository,),
        )
        results = WebhookIngestor(store, sources=(source,)).process_pending()

        assert len(results) == 1
        assert not results[0].accepted
        assert "OpenSpec" in (results[0].reason or "")
        assert store.list_queued_requests() == []
        assert store.list_received_webhook_events() == []
        assert store.list_webhook_events()[0].event_id == "monitor-alert-1"


def test_ingestion_retry_preserves_request_triage_after_partial_processing(
    tmp_path: Path,
) -> None:
    repository = create_repository(tmp_path / "repository")
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        accepted_alert(store, repository)
        stable_request_id = str(
            uuid5(NAMESPACE_URL, "ai-software-factory:monitor:monitor-alert-1")
        )
        request = intake_request(
            repository,
            "A production alert needs investigation",
            source=RequestSource.MONITORING,
            request_id=stable_request_id,
        )
        store.enqueue_request_once(request)
        store.update_request(
            request.model_copy(update={"triage_outcome": TriageOutcome.ACTIONABLE})
        )
        source = WebhookSource(
            source_id="monitor",
            secret=SecretStr(WEBHOOK_SECRET),
            allowed_repositories=(repository,),
        )

        result = WebhookIngestor(store, sources=(source,)).process_pending()[0]
        preserved = store.list_queued_requests()[0]

        assert result.accepted and result.duplicate_request
        assert preserved.triage_outcome is TriageOutcome.ACTIONABLE
        assert store.list_received_webhook_events() == []


def test_webhook_ingestion_is_idempotent_across_reprocessing(tmp_path: Path) -> None:
    repository = create_repository(tmp_path / "repository")
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        accepted_alert(store, repository)
        source = WebhookSource(
            source_id="monitor",
            secret=SecretStr(WEBHOOK_SECRET),
            allowed_repositories=(repository,),
        )
        ingestor = WebhookIngestor(store, sources=(source,))
        first = ingestor.process_pending()
        second = ingestor.process_pending()

        assert len(first) == 1 and first[0].accepted
        assert second == []
        assert len(store.list_queued_requests()) == 1
