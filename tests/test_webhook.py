import hashlib
import hmac
import json
from datetime import UTC, datetime, timedelta
from io import BytesIO
from pathlib import Path

from pydantic import SecretStr

from cronos_ai.storage import FactoryStore
from cronos_ai.webhook import WebhookEndpoint, WebhookSource

TEST_SECRET = "test-only-secret-longer-than-thirty-two-bytes-12345"
OTHER_TEST_SECRET = "other-test-secret-longer-than-thirty-two-bytes-12345"
ATTACKER_SECRET = "attacker-only-secret-longer-than-thirty-two-bytes"


def signed_headers(
    body: bytes,
    *,
    secret: str,
    event_id: str = "alert-123",
    source_id: str = "monitor",
    timestamp: datetime | None = None,
    signature_override: str | None = None,
) -> dict[str, str]:
    signed_at = timestamp or datetime.now(UTC)
    timestamp_value = str(int(signed_at.timestamp()))
    digest = hmac.new(
        secret.encode("utf-8"),
        timestamp_value.encode("ascii")
        + b"."
        + source_id.encode("ascii")
        + b"."
        + event_id.encode("utf-8")
        + b"."
        + body,
        hashlib.sha256,
    ).hexdigest()
    return {
        "X-Factory-Timestamp": timestamp_value,
        "X-Factory-Event-Id": event_id,
        "X-Factory-Signature": signature_override or f"sha256={digest}",
    }


def alert_body(
    repository: Path, description: str = "Investigate a production alert"
) -> bytes:
    return json.dumps(
        {"repository": str(repository), "description": description}
    ).encode("utf-8")


def make_endpoint(
    database: Path,
    *,
    secret: str = TEST_SECRET,
) -> tuple[FactoryStore, WebhookEndpoint]:
    store = FactoryStore(database)
    endpoint = WebhookEndpoint(
        store,
        sources=(
            WebhookSource(
                source_id="monitor",
                secret=SecretStr(secret),
                allowed_repositories=(database.parent / "target-repository",),
            ),
        ),
    )
    return store, endpoint


def test_valid_webhook_is_authenticated_validated_and_deduplicated(
    tmp_path: Path,
) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(body, secret=TEST_SECRET, timestamp=now)
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    accepted = endpoint.handle("monitor", headers, body, now=now)
    duplicate = endpoint.handle("monitor", headers, body, now=now)

    events = store.list_webhook_events()
    assert accepted.status_code == 202
    assert duplicate.status_code == 200
    assert duplicate.duplicate
    assert len(events) == 1
    assert events[0].event_id == "alert-123"
    assert events[0].payload.repository == str(tmp_path / "target-repository")
    assert store.list_received_webhook_events() == events
    store.mark_webhook_event_processed("monitor", "alert-123")
    assert store.list_received_webhook_events() == []
    assert store.list_run_ids() == []
    store.close()


def test_invalid_signature_does_not_persist_event_or_create_work(
    tmp_path: Path,
) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(body, secret=ATTACKER_SECRET, timestamp=now)
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    response = endpoint.handle("monitor", headers, body, now=now)

    assert response.status_code == 401
    assert store.list_webhook_events() == []
    assert store.list_run_ids() == []
    store.close()


def test_event_identifier_is_covered_by_signature(tmp_path: Path) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(body, secret=TEST_SECRET, timestamp=now)
    headers["X-Factory-Event-Id"] = "forged-event-id"
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    response = endpoint.handle("monitor", headers, body, now=now)

    assert response.status_code == 401
    assert store.list_webhook_events() == []
    store.close()


def test_signature_is_bound_to_the_source_route(tmp_path: Path) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(
        body, secret=OTHER_TEST_SECRET, source_id="monitor", timestamp=now
    )
    store = FactoryStore(tmp_path / "factory.sqlite3")
    endpoint = WebhookEndpoint(
        store,
        sources=(
            WebhookSource(
                source_id="monitor",
                secret=SecretStr(TEST_SECRET),
                allowed_repositories=(tmp_path / "target-repository",),
            ),
            WebhookSource(
                source_id="monitor-two",
                secret=SecretStr(OTHER_TEST_SECRET),
                allowed_repositories=(tmp_path / "target-repository",),
            ),
        ),
    )

    response = endpoint.handle("monitor-two", headers, body, now=now)

    assert response.status_code == 401
    assert store.list_webhook_events() == []
    store.close()


def test_stale_timestamp_is_rejected_before_event_persistence(tmp_path: Path) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(
        body,
        secret=TEST_SECRET,
        timestamp=now - timedelta(minutes=10),
    )
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    response = endpoint.handle("monitor", headers, body, now=now)

    assert response.status_code == 401
    assert store.list_webhook_events() == []
    store.close()


def test_reused_event_identifier_with_different_body_is_rejected(
    tmp_path: Path,
) -> None:
    now = datetime.now(UTC)
    original = alert_body(tmp_path / "target-repository", "First alert")
    changed = alert_body(tmp_path / "target-repository", "Changed content")
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    first = endpoint.handle(
        "monitor",
        signed_headers(original, secret=TEST_SECRET, timestamp=now),
        original,
        now=now,
    )
    replay = endpoint.handle(
        "monitor",
        signed_headers(changed, secret=TEST_SECRET, timestamp=now),
        changed,
        now=now,
    )

    assert first.status_code == 202
    assert replay.status_code == 409
    assert len(store.list_webhook_events()) == 1
    store.close()


def test_source_cannot_target_a_repository_outside_its_allowlist(
    tmp_path: Path,
) -> None:
    body = alert_body(tmp_path / "other-repository")
    now = datetime.now(UTC)
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    response = endpoint.handle(
        "monitor",
        signed_headers(body, secret=TEST_SECRET, timestamp=now),
        body,
        now=now,
    )

    assert response.status_code == 403
    assert store.list_webhook_events() == []
    store.close()


def test_payload_cannot_smuggle_commands_or_unknown_fields(tmp_path: Path) -> None:
    body = json.dumps(
        {
            "repository": str(tmp_path / "target-repository"),
            "description": "Investigate this alert",
            "command": "rm -rf /",
        }
    ).encode("utf-8")
    now = datetime.now(UTC)
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")

    response = endpoint.handle(
        "monitor",
        signed_headers(body, secret=TEST_SECRET, timestamp=now),
        body,
        now=now,
    )

    assert response.status_code == 400
    assert store.list_webhook_events() == []
    assert store.list_run_ids() == []
    store.close()


def test_invalid_schema_and_oversized_body_are_rejected(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    invalid = b'{"repository":"relative/path","description":"alert"}'
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")
    invalid_response = endpoint.handle(
        "monitor",
        signed_headers(invalid, secret=TEST_SECRET, timestamp=now),
        invalid,
        now=now,
    )
    oversized = b"x" * (endpoint.max_body_bytes + 1)
    oversized_response = endpoint.handle(
        "monitor",
        signed_headers(oversized, secret=TEST_SECRET, timestamp=now),
        oversized,
        now=now,
    )

    assert invalid_response.status_code == 400
    assert oversized_response.status_code == 413
    assert store.list_webhook_events() == []
    store.close()


def test_wsgi_route_exposes_only_configured_post_endpoint(tmp_path: Path) -> None:
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)
    headers = signed_headers(body, secret=TEST_SECRET, timestamp=now)
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")
    response_status: list[str] = []
    environ = {
        "REQUEST_METHOD": "POST",
        "PATH_INFO": "/webhooks/monitor",
        "CONTENT_LENGTH": str(len(body)),
        "CONTENT_TYPE": "application/json; charset=utf-8",
        "HTTP_X_FACTORY_TIMESTAMP": headers["X-Factory-Timestamp"],
        "HTTP_X_FACTORY_EVENT_ID": headers["X-Factory-Event-Id"],
        "HTTP_X_FACTORY_SIGNATURE": headers["X-Factory-Signature"],
        "wsgi.input": BytesIO(body),
    }

    response = endpoint(
        environ, lambda status, _headers: response_status.append(status)
    )
    payload = json.loads(response[0])

    assert response_status == ["202 Accepted"]
    assert payload["status"] == "Webhook event accepted"
    assert len(store.list_webhook_events()) == 1

    environ["CONTENT_TYPE"] = "text/plain"
    endpoint(environ, lambda status, _headers: response_status.append(status))
    assert response_status[-1] == "415 Unsupported Media Type"
    assert len(store.list_webhook_events()) == 1
    store.close()


def test_unconfigured_source_is_not_accepted(tmp_path: Path) -> None:
    store, endpoint = make_endpoint(tmp_path / "factory.sqlite3")
    body = alert_body(tmp_path / "target-repository")
    now = datetime.now(UTC)

    response = endpoint.handle(
        "unknown",
        signed_headers(body, secret=TEST_SECRET, timestamp=now),
        body,
        now=now,
    )

    assert response.status_code == 404
    assert store.list_webhook_events() == []
    store.close()
