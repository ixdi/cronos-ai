"""Provider-neutral, authenticated WSGI webhook ingress for alert events."""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import sqlite3
from datetime import UTC, datetime
from io import BytesIO
from pathlib import Path
from typing import Any, Protocol

from pydantic import Field, SecretStr, ValidationError, model_validator

from cronos_ai.models import (
    NonEmptyString,
    ValidatedModel,
    WebhookAlertPayload,
    WebhookEvent,
    WebhookReceipt,
)
from cronos_ai.storage import FactoryStore, StorageError


class WebhookSource(ValidatedModel):
    """One explicitly configured source and its shared signing secret."""

    source_id: NonEmptyString
    secret: SecretStr
    allowed_repositories: tuple[Path, ...] = Field(min_length=1)
    max_age_seconds: int = 300

    @model_validator(mode="after")
    def validate_source(self) -> WebhookSource:
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,31}", self.source_id):
            raise ValueError("webhook source ID has an invalid format")
        if len(self.secret.get_secret_value().encode("utf-8")) < 32:
            raise ValueError("webhook secrets must contain at least 32 bytes")
        if any(not path.is_absolute() for path in self.allowed_repositories):
            raise ValueError("webhook repository allowlist paths must be absolute")
        resolved_repositories = [path.resolve() for path in self.allowed_repositories]
        if len(resolved_repositories) != len(set(resolved_repositories)):
            raise ValueError("webhook repository allowlist must not contain duplicates")
        if self.max_age_seconds < 1 or self.max_age_seconds > 3600:
            raise ValueError(
                "webhook timestamp window must be between 1 and 3600 seconds"
            )
        return self


class WebhookResponse(ValidatedModel):
    status_code: int
    message: NonEmptyString
    duplicate: bool = False


class StartResponse(Protocol):
    def __call__(self, status: str, headers: list[tuple[str, str]]) -> None: ...


class WebhookEndpoint:
    """Authenticate, validate, deduplicate, and persist signed alert events."""

    DEFAULT_MAX_BODY_BYTES = 65_536
    _STATUS_TEXT = {
        200: "OK",
        202: "Accepted",
        400: "Bad Request",
        401: "Unauthorized",
        404: "Not Found",
        405: "Method Not Allowed",
        403: "Forbidden",
        409: "Conflict",
        413: "Payload Too Large",
        415: "Unsupported Media Type",
        503: "Service Unavailable",
    }

    def __init__(
        self,
        store: FactoryStore,
        *,
        sources: tuple[WebhookSource, ...],
        max_body_bytes: int = DEFAULT_MAX_BODY_BYTES,
    ) -> None:
        if max_body_bytes < 1:
            raise ValueError("webhook body limit must be positive")
        source_map = {source.source_id: source for source in sources}
        if len(source_map) != len(sources):
            raise ValueError("webhook source IDs must be unique")
        secrets = [source.secret.get_secret_value() for source in sources]
        if len(secrets) != len(set(secrets)):
            raise ValueError("each webhook source must use a distinct secret")
        self.store = store
        self.sources = source_map
        self.max_body_bytes = max_body_bytes

    def handle(
        self,
        source_id: str,
        headers: dict[str, str],
        body: bytes,
        *,
        now: datetime | None = None,
    ) -> WebhookResponse:
        source = self.sources.get(source_id)
        if source is None:
            return WebhookResponse(status_code=404, message="Unknown webhook source")
        if len(body) > self.max_body_bytes:
            return WebhookResponse(status_code=413, message="Webhook body is too large")

        normalized_headers = {
            key.casefold(): value.strip() for key, value in headers.items()
        }
        timestamp_value = normalized_headers.get("x-factory-timestamp", "")
        event_id = normalized_headers.get("x-factory-event-id", "")
        supplied_signature = normalized_headers.get("x-factory-signature", "")
        if (
            not timestamp_value.isascii()
            or not timestamp_value.isdigit()
            or not event_id
            or not supplied_signature.startswith("sha256=")
        ):
            return WebhookResponse(
                status_code=401, message="Webhook authentication failed"
            )
        signature_hex = supplied_signature.removeprefix("sha256=")
        if not re.fullmatch(r"[a-f0-9]{64}", signature_hex):
            return WebhookResponse(
                status_code=401, message="Webhook authentication failed"
            )

        try:
            timestamp_seconds = int(timestamp_value)
            signed_at = datetime.fromtimestamp(timestamp_seconds, UTC)
        except (OverflowError, OSError, ValueError):
            return WebhookResponse(
                status_code=401, message="Webhook authentication failed"
            )
        current_time = now or datetime.now(UTC)
        if current_time.tzinfo is None or current_time.utcoffset() is None:
            return WebhookResponse(status_code=400, message="Invalid receipt timestamp")
        age = abs((current_time - signed_at).total_seconds())
        if age > source.max_age_seconds:
            return WebhookResponse(
                status_code=401,
                message="Webhook timestamp is outside the allowed window",
            )

        signing_key = source.secret.get_secret_value().encode("utf-8")
        expected_signature = hmac.new(
            signing_key,
            timestamp_value.encode("ascii")
            + b"."
            + source_id.encode("ascii")
            + b"."
            + event_id.encode("utf-8")
            + b"."
            + body,
            hashlib.sha256,
        ).hexdigest()
        if not hmac.compare_digest(signature_hex, expected_signature):
            return WebhookResponse(
                status_code=401, message="Webhook authentication failed"
            )

        try:
            payload_data = json.loads(body.decode("utf-8"))
            payload = WebhookAlertPayload.model_validate(payload_data)
            source_repositories = {
                repository.resolve() for repository in source.allowed_repositories
            }
            if Path(payload.repository).resolve() not in source_repositories:
                return WebhookResponse(
                    status_code=403,
                    message="Repository is not allowed for this webhook source",
                )
            event = WebhookEvent(
                source_id=source_id,
                event_id=event_id,
                timestamp=signed_at,
                body_sha256=hashlib.sha256(body).hexdigest(),
                payload=payload,
                received_at=current_time,
            )
        except (UnicodeDecodeError, json.JSONDecodeError, ValidationError, ValueError):
            return WebhookResponse(
                status_code=400, message="Invalid webhook event schema"
            )

        try:
            receipt = self.store.record_webhook_event(event)
        except (StorageError, sqlite3.Error):
            return WebhookResponse(
                status_code=503, message="Webhook receipt is unavailable"
            )
        if receipt is WebhookReceipt.CONFLICT:
            return WebhookResponse(
                status_code=409,
                message="Webhook event ID was already used for different content",
            )
        if receipt is WebhookReceipt.DUPLICATE:
            return WebhookResponse(
                status_code=200,
                message="Webhook event was already accepted",
                duplicate=True,
            )
        return WebhookResponse(status_code=202, message="Webhook event accepted")

    def __call__(
        self, environ: dict[str, Any], start_response: StartResponse
    ) -> list[bytes]:
        """Expose the handler as a bounded WSGI application for private hosting."""
        method = environ.get("REQUEST_METHOD", "").upper()
        path = environ.get("PATH_INFO", "")
        if method != "POST":
            response = WebhookResponse(status_code=405, message="POST is required")
        elif not path.startswith("/webhooks/") or "/" in path.removeprefix(
            "/webhooks/"
        ):
            response = WebhookResponse(
                status_code=404, message="Webhook route not found"
            )
        else:
            source_id = path.removeprefix("/webhooks/")
            try:
                content_length = int(environ.get("CONTENT_LENGTH", ""))
            except (TypeError, ValueError):
                content_length = -1
            if content_length < 0:
                response = WebhookResponse(
                    status_code=400, message="Invalid content length"
                )
            elif content_length > self.max_body_bytes:
                response = WebhookResponse(
                    status_code=413, message="Webhook body is too large"
                )
            elif (
                str(environ.get("CONTENT_TYPE", "")).split(";", 1)[0].strip().casefold()
                != "application/json"
            ):
                response = WebhookResponse(
                    status_code=415, message="application/json is required"
                )
            else:
                stream = environ.get("wsgi.input", BytesIO())
                body = stream.read(content_length)
                headers = {
                    "x-factory-timestamp": str(
                        environ.get("HTTP_X_FACTORY_TIMESTAMP", "")
                    ),
                    "x-factory-event-id": str(
                        environ.get("HTTP_X_FACTORY_EVENT_ID", "")
                    ),
                    "x-factory-signature": str(
                        environ.get("HTTP_X_FACTORY_SIGNATURE", "")
                    ),
                }
                response = self.handle(source_id, headers, body)

        response_body = json.dumps(
            {"status": response.message, "duplicate": response.duplicate}
        ).encode("utf-8")
        status = f"{response.status_code} {self._STATUS_TEXT[response.status_code]}"
        start_response(
            status,
            [
                ("Content-Type", "application/json; charset=utf-8"),
                ("Content-Length", str(len(response_body))),
                ("Cache-Control", "no-store"),
                ("X-Content-Type-Options", "nosniff"),
            ],
        )
        return [response_body]
