"""Normalize authenticated alerts into ordinary, non-dispatchable requests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from uuid import NAMESPACE_URL, uuid5

from cronos_ai.intake import IntakeError, intake_request
from cronos_ai.models import RequestSource
from cronos_ai.storage import FactoryStore
from cronos_ai.webhook import WebhookSource


@dataclass(frozen=True)
class WebhookIngestionResult:
    """Outcome for one accepted webhook event entering ordinary intake."""

    source_id: str
    event_id: str
    request_id: str | None
    accepted: bool
    duplicate_request: bool = False
    reason: str | None = None


class WebhookIngestor:
    """Process authenticated alerts through the same repository intake checks."""

    def __init__(
        self,
        store: FactoryStore,
        *,
        sources: tuple[WebhookSource, ...],
    ) -> None:
        source_map = {source.source_id: source for source in sources}
        if len(source_map) != len(sources):
            raise ValueError("webhook source IDs must be unique")
        self.store = store
        self.sources = source_map

    def process_pending(self) -> list[WebhookIngestionResult]:
        """Convert only authenticated, allowlisted events into queued requests."""
        results: list[WebhookIngestionResult] = []
        for event in self.store.list_received_webhook_events():
            source = self.sources.get(event.source_id)
            if source is None:
                self.store.mark_webhook_event_processed(
                    event.source_id, event.event_id, rejected=True
                )
                results.append(
                    WebhookIngestionResult(
                        source_id=event.source_id,
                        event_id=event.event_id,
                        request_id=None,
                        accepted=False,
                        reason="Webhook source is no longer configured.",
                    )
                )
                continue

            try:
                repository = Path(event.payload.repository).resolve(strict=True)
                allowed_repositories = {
                    path.resolve() for path in source.allowed_repositories
                }
                if repository not in allowed_repositories:
                    raise IntakeError(
                        "alert repository is outside the source allowlist"
                    )
                request_id = str(
                    uuid5(
                        NAMESPACE_URL,
                        f"ai-software-factory:{event.source_id}:{event.event_id}",
                    )
                )
                request = intake_request(
                    repository,
                    event.payload.description,
                    source=RequestSource.MONITORING,
                    request_id=request_id,
                )
            except (IntakeError, OSError, RuntimeError) as error:
                self.store.mark_webhook_event_processed(
                    event.source_id, event.event_id, rejected=True
                )
                results.append(
                    WebhookIngestionResult(
                        source_id=event.source_id,
                        event_id=event.event_id,
                        request_id=None,
                        accepted=False,
                        reason=str(error),
                    )
                )
                continue

            inserted = self.store.enqueue_request_once(request)
            self.store.mark_webhook_event_processed(event.source_id, event.event_id)
            results.append(
                WebhookIngestionResult(
                    source_id=event.source_id,
                    event_id=event.event_id,
                    request_id=request.request_id,
                    accepted=True,
                    duplicate_request=not inserted,
                )
            )
        return results
