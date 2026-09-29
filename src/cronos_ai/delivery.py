"""Provider-neutral CI/CD dispatch contract and human-review delivery gate."""

from __future__ import annotations

import hashlib
from datetime import UTC, datetime
from typing import Protocol

from cronos_ai.models import (
    CICDRequest,
    CICDResult,
    DeliveryRecord,
    DeliveryStatus,
    ReviewPacket,
)
from cronos_ai.review import ReviewCoordinator, ReviewError
from cronos_ai.storage import FactoryStore


class DeliveryError(RuntimeError):
    """Raised when delivery is unconfigured or cannot safely proceed."""


class CIAdapter(Protocol):
    """Provider-neutral contract for one idempotent workflow dispatch and polling."""

    def dispatch(self, request: CICDRequest) -> CICDResult: ...

    def status(self, external_id: str) -> CICDResult: ...


class FakeCIAdapter:
    """Deterministic fake with configurable dispatch and subsequent statuses."""

    def __init__(
        self,
        *,
        dispatch_status: DeliveryStatus = DeliveryStatus.INCONCLUSIVE,
        status_results: tuple[DeliveryStatus, ...] = (),
    ) -> None:
        self.dispatch_status = dispatch_status
        self.status_results = status_results
        self.dispatch_calls = 0
        self.status_calls = 0
        self.requests: list[CICDRequest] = []
        self._results_by_key: dict[str, CICDResult] = {}
        self._status_index: dict[str, int] = {}

    def dispatch(self, request: CICDRequest) -> CICDResult:
        self.dispatch_calls += 1
        existing = self._results_by_key.get(request.idempotency_key)
        if existing is not None:
            return existing
        external_id = "fake-ci-" + request.idempotency_key[:16]
        result = CICDResult(
            external_id=external_id,
            status=self.dispatch_status,
            summary=f"Fake CI workflow {self.dispatch_status.value}.",
        )
        self.requests.append(request)
        self._results_by_key[request.idempotency_key] = result
        self._status_index[external_id] = 0
        return result

    def status(self, external_id: str) -> CICDResult:
        self.status_calls += 1
        matches = [
            result
            for result in self._results_by_key.values()
            if result.external_id == external_id
        ]
        if not matches:
            return CICDResult(
                external_id=external_id,
                status=DeliveryStatus.INCONCLUSIVE,
                summary="Fake CI does not recognize this workflow ID.",
            )
        index = self._status_index[external_id]
        if self.status_results:
            status_index = min(index, len(self.status_results) - 1)
            state = self.status_results[status_index]
            self._status_index[external_id] = index + 1
        else:
            state = matches[0].status
        result = CICDResult(
            external_id=external_id,
            status=state,
            summary=f"Fake CI workflow {state.value}.",
        )
        self._results_by_key[
            next(
                key
                for key, existing in self._results_by_key.items()
                if existing.external_id == external_id
            )
        ] = result
        return result


class DeliveryCoordinator:
    """Dispatch only an unchanged review approved by a human."""

    def __init__(
        self,
        store: FactoryStore,
        *,
        adapter: CIAdapter | None,
    ) -> None:
        self.store = store
        self.adapter = adapter
        self.review_gate = ReviewCoordinator(store)

    def dispatch(self, run_id: str) -> DeliveryRecord:
        """Create one idempotent delivery attempt for the current approved packet."""
        adapter = self._require_adapter()
        try:
            packet = self.review_gate.require_delivery_approval(run_id)
        except ReviewError as error:
            raise DeliveryError(str(error)) from error
        request = self._delivery_request(run_id, packet)
        existing = self.store.get_delivery_record(run_id, packet.review_hash)
        if existing is not None and existing.status is not DeliveryStatus.PENDING:
            return existing
        if existing is None:
            now = datetime.now(UTC)
            existing = DeliveryRecord(
                request=request,
                status=DeliveryStatus.PENDING,
                summary="Delivery dispatch is pending.",
                created_at=now,
                updated_at=now,
            )
            self.store.create_delivery_record(existing)

        try:
            result = adapter.dispatch(request)
        except Exception:
            result = CICDResult(
                status=DeliveryStatus.INCONCLUSIVE,
                summary="CI/CD adapter returned no conclusive dispatch result.",
            )
        updated = self._record_result(existing, result)
        self.store.update_delivery_record(updated)
        return updated

    def refresh_status(self, run_id: str) -> DeliveryRecord:
        """Poll an active workflow without treating unknown outcomes as success."""
        adapter = self._require_adapter()
        try:
            packet = self.review_gate.require_delivery_approval(run_id)
        except ReviewError as error:
            raise DeliveryError(str(error)) from error
        record = self.store.get_delivery_record(run_id, packet.review_hash)
        if record is None:
            raise DeliveryError("no CI/CD dispatch exists for the approved review")
        if record.status not in (DeliveryStatus.PENDING, DeliveryStatus.RUNNING):
            return record
        if record.external_id is None:
            return self._update_inconclusive(
                record,
                "CI/CD adapter did not provide a workflow ID for status polling.",
            )
        try:
            result = adapter.status(record.external_id)
        except Exception:
            return self._update_inconclusive(
                record,
                "CI/CD adapter returned no conclusive workflow status.",
            )
        if result.external_id != record.external_id:
            return self._update_inconclusive(
                record,
                "CI/CD adapter returned a mismatched workflow ID.",
            )
        updated = self._record_result(record, result)
        self.store.update_delivery_record(updated)
        return updated

    def is_delivered(self, run_id: str) -> bool:
        """Report delivery only for a successful workflow on the approved revision."""
        packet = self.store.get_latest_review_packet(run_id)
        if packet is None or not self.review_gate.can_deliver(run_id):
            return False
        record = self.store.get_delivery_record(run_id, packet.review_hash)
        return record is not None and record.status is DeliveryStatus.SUCCEEDED

    def _delivery_request(self, run_id: str, packet: ReviewPacket) -> CICDRequest:
        run = self.store.get_run(run_id)
        context = self.store.get_run_context(run_id)
        if run is None or context is None:
            raise DeliveryError("run is missing its persisted repository context")
        request, _ = run
        key_material = f"{run_id}:{packet.review_hash}".encode()
        return CICDRequest(
            run_id=run_id,
            repository=request.repo_path,
            branch_name=context.branch_name,
            revision=packet.head_commit,
            review_hash=packet.review_hash,
            idempotency_key=hashlib.sha256(key_material).hexdigest(),
        )

    def _record_result(
        self,
        record: DeliveryRecord,
        result: CICDResult,
    ) -> DeliveryRecord:
        if result.status in (DeliveryStatus.RUNNING, DeliveryStatus.SUCCEEDED):
            if result.external_id is None:
                return self._record_result(
                    record,
                    CICDResult(
                        status=DeliveryStatus.INCONCLUSIVE,
                        summary="CI/CD adapter omitted the workflow ID.",
                    ),
                )
        return record.model_copy(
            update={
                "status": result.status,
                "external_id": result.external_id,
                "summary": result.summary,
                "updated_at": datetime.now(UTC),
            }
        )

    def _update_inconclusive(
        self, record: DeliveryRecord, summary: str
    ) -> DeliveryRecord:
        updated = self._record_result(
            record,
            CICDResult(status=DeliveryStatus.INCONCLUSIVE, summary=summary),
        )
        self.store.update_delivery_record(updated)
        return updated

    def _require_adapter(self) -> CIAdapter:
        if self.adapter is None:
            raise DeliveryError("CI/CD adapter is not configured")
        return self.adapter
