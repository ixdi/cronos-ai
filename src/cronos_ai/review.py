"""Integrated-diff review evidence and fail-closed delivery approval gate."""

from __future__ import annotations

import hashlib
import json
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

from cronos_ai.models import (
    ReviewPacket,
    ReviewState,
    TaskRecord,
    TaskState,
    UserScenarioCheck,
    VerificationCheck,
)
from cronos_ai.storage import FactoryStore, StorageError


class ReviewError(ValueError):
    """Raised when a review packet or delivery decision is unsafe or incomplete."""


class ReviewCoordinator:
    """Capture immutable evidence and require human approval of its exact version."""

    def __init__(self, store: FactoryStore, *, max_attempts: int = 3) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        self.store = store
        self.max_attempts = max_attempts

    def prepare_review(
        self,
        run_id: str,
        *,
        review_summary: str,
        verification_checks: tuple[VerificationCheck, ...],
        user_scenarios: tuple[UserScenarioCheck, ...],
        review_findings: tuple[str, ...] = (),
        blocking_findings: tuple[str, ...] = (),
    ) -> ReviewPacket:
        """Capture the integrated diff and passing automated/user evidence."""
        if not verification_checks or not user_scenarios:
            raise ReviewError("review requires automated and user-perspective evidence")
        if not review_summary.strip():
            raise ReviewError("review summary must not be empty")

        run = self.store.get_run(run_id)
        if run is None:
            raise ReviewError(f"run does not exist: {run_id}")
        context = self.store.get_run_context(run_id)
        if context is None or context.starting_commit is None:
            raise ReviewError("run is missing its starting commit and worktree context")
        for task in self.store.list_tasks(run_id):
            if task.state not in (TaskState.REVIEW, TaskState.DONE):
                raise ReviewError(
                    f"task {task.task_id} is not integrated and ready for run review"
                )

        base_commit, head_commit, diff_text = self._capture_diff(run_id)
        if not diff_text.strip():
            raise ReviewError("integrated diff is empty")
        diff_hash = hashlib.sha256(diff_text.encode("utf-8")).hexdigest()
        all_blocking_findings = list(blocking_findings)
        all_blocking_findings.extend(
            f"Automated check failed: {check.name}: {check.summary}"
            for check in verification_checks
            if not check.passed
        )
        all_blocking_findings.extend(
            f"User scenario failed: {scenario.scenario}: {scenario.evidence}"
            for scenario in user_scenarios
            if not scenario.passed
        )
        packet_state = (
            ReviewState.BLOCKED if all_blocking_findings else ReviewState.READY
        )
        review_hash = self._review_fingerprint(
            run_id,
            base_commit,
            head_commit,
            diff_hash,
            review_summary.strip(),
            review_findings,
            verification_checks,
            user_scenarios,
            tuple(all_blocking_findings),
        )
        packet = ReviewPacket(
            review_id=str(uuid4()),
            run_id=run_id,
            base_commit=base_commit,
            head_commit=head_commit,
            diff_text=diff_text,
            diff_hash=diff_hash,
            review_hash=review_hash,
            review_summary=review_summary.strip(),
            review_findings=review_findings,
            verification_checks=verification_checks,
            user_scenarios=user_scenarios,
            blocking_findings=tuple(all_blocking_findings),
            state=packet_state,
        )
        if packet.state is ReviewState.BLOCKED:
            self.store.create_blocked_review_packet(
                packet,
                self._tasks_after_failed_verification(run_id, packet.blocking_findings),
            )
        else:
            self.store.create_review_packet(packet)
        return packet

    def decide_review(
        self,
        run_id: str,
        *,
        review_hash: str,
        reviewer: str,
        approve: bool,
        rationale: str | None = None,
    ) -> ReviewPacket:
        """Record a human decision only for the current, unchanged review packet."""
        packet = self.store.get_latest_review_packet(run_id)
        if packet is None:
            raise ReviewError("run has no review packet")
        desired_state = ReviewState.APPROVED if approve else ReviewState.REJECTED
        if packet.review_hash != review_hash:
            raise ReviewError("review decision does not match the current packet")
        if packet.state is desired_state and packet.reviewer == reviewer:
            return packet
        if packet.state is not ReviewState.READY:
            raise ReviewError("review packet is not awaiting a human decision")
        if not approve and not (rationale and rationale.strip()):
            raise ReviewError("rejected reviews require a rationale")
        _, current_head, current_diff = self._capture_diff(run_id)
        current_diff_hash = hashlib.sha256(current_diff.encode("utf-8")).hexdigest()
        if current_diff_hash != packet.diff_hash or current_head != packet.head_commit:
            raise ReviewError(
                "integrated diff changed after the review packet was created"
            )

        decided_at = datetime.now(UTC)
        decided = packet.model_copy(
            update={
                "state": desired_state,
                "reviewer": reviewer,
                "rationale": rationale.strip() if rationale else None,
                "decided_at": decided_at,
            }
        )
        updated_tasks = self._tasks_after_decision(
            run_id,
            approve=approve,
            rationale=rationale,
        )
        self.store.record_review_decision(decided, updated_tasks)
        return decided

    def can_deliver(self, run_id: str) -> bool:
        """Return true only for an approved packet matching the current diff."""
        packet = self.store.get_latest_review_packet(run_id)
        if packet is None or packet.state is not ReviewState.APPROVED:
            return False
        if (
            any(not check.passed for check in packet.verification_checks)
            or any(not scenario.passed for scenario in packet.user_scenarios)
            or packet.blocking_findings
        ):
            return False
        run = self.store.get_run(run_id)
        if run is None:
            return False
        _, plan = run
        tasks = {task.task_id: task for task in self.store.list_tasks(run_id)}
        if any(
            tasks.get(task.task_id) is None
            or tasks[task.task_id].state is not TaskState.DONE
            for task in plan.tasks
        ):
            return False
        try:
            _, current_head, current_diff = self._capture_diff(run_id)
        except ReviewError:
            return False
        return (
            current_head == packet.head_commit
            and hashlib.sha256(current_diff.encode("utf-8")).hexdigest()
            == packet.diff_hash
        )

    def require_delivery_approval(self, run_id: str) -> ReviewPacket:
        """Raise unless the exact integrated diff has human approval."""
        packet = self.store.get_latest_review_packet(run_id)
        if packet is None or not self.can_deliver(run_id):
            raise ReviewError("human-approved review is required before delivery")
        return packet

    def _capture_diff(self, run_id: str) -> tuple[str, str, str]:
        run = self.store.get_run(run_id)
        context = self.store.get_run_context(run_id)
        if run is None or context is None or context.starting_commit is None:
            raise ReviewError("run is missing its starting commit and worktree context")
        request, _ = run
        worktree = context.run_worktree_path
        try:
            root = self._git(worktree, "rev-parse", "--show-toplevel").strip()
            branch = self._git(worktree, "branch", "--show-current").strip()
            common_dir = Path(
                self._git(
                    worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ).strip()
            ).resolve()
            target_common_dir = Path(
                self._git(
                    request.repo_path,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ).strip()
            ).resolve()
            if Path(root).resolve() != worktree.resolve():
                raise ReviewError("review worktree is not a Git root")
            if branch != context.branch_name or common_dir != target_common_dir:
                raise ReviewError("review worktree does not match the registered run")
            if self._git(
                worktree, "status", "--porcelain=v1", "--untracked-files=all"
            ).strip():
                raise ReviewError("run worktree must be clean before review")
            head = self._git(worktree, "rev-parse", "HEAD").strip()
            self._git(
                worktree, "merge-base", "--is-ancestor", context.starting_commit, head
            )
            diff_text = self._git(
                worktree,
                "diff",
                "--binary",
                f"{context.starting_commit}..{head}",
            )
        except (OSError, StorageError) as error:
            raise ReviewError(
                "could not inspect the integrated run worktree"
            ) from error
        except RuntimeError as error:
            raise ReviewError(str(error)) from error
        return context.starting_commit, head, diff_text

    @staticmethod
    def _git(repository: Path, *args: str) -> str:
        result = subprocess.run(
            ["git", *args],
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            details = result.stderr.strip() or result.stdout.strip()
            raise RuntimeError(f"Git command failed: {' '.join(args)}: {details}")
        return result.stdout

    @staticmethod
    def _review_fingerprint(
        run_id: str,
        base_commit: str,
        head_commit: str,
        diff_hash: str,
        review_summary: str,
        review_findings: tuple[str, ...],
        verification_checks: tuple[VerificationCheck, ...],
        user_scenarios: tuple[UserScenarioCheck, ...],
        blocking_findings: tuple[str, ...],
    ) -> str:
        body = {
            "run_id": run_id,
            "base_commit": base_commit,
            "head_commit": head_commit,
            "diff_hash": diff_hash,
            "review_summary": review_summary,
            "review_findings": review_findings,
            "verification_checks": [
                check.model_dump(mode="json") for check in verification_checks
            ],
            "user_scenarios": [
                scenario.model_dump(mode="json") for scenario in user_scenarios
            ],
            "blocking_findings": blocking_findings,
        }
        canonical = json.dumps(body, sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(canonical.encode("utf-8")).hexdigest()

    def _tasks_after_failed_verification(
        self,
        run_id: str,
        findings: tuple[str, ...],
    ) -> tuple[TaskRecord, ...]:
        reason = "Review verification requires remediation: " + "; ".join(findings)
        updates: list[TaskRecord] = []
        for task in self.store.list_tasks(run_id):
            if task.state is not TaskState.REVIEW:
                continue
            state = (
                TaskState.BLOCKED
                if task.attempt_count >= self.max_attempts
                else TaskState.READY
            )
            task_reason = (
                "Review remediation attempt limit exhausted."
                if state is TaskState.BLOCKED
                else reason
            )
            updates.append(
                task.model_copy(
                    update={"state": state, "reason": task_reason, "worker_id": None}
                )
            )
        return tuple(updates)

    def _tasks_after_decision(
        self,
        run_id: str,
        *,
        approve: bool,
        rationale: str | None,
    ) -> tuple[TaskRecord, ...]:
        changes: list[TaskRecord] = []
        for task in self.store.list_tasks(run_id):
            if task.state is not TaskState.REVIEW:
                continue
            if approve:
                updated = task.model_copy(
                    update={"state": TaskState.DONE, "reason": None, "worker_id": None}
                )
            elif task.attempt_count >= self.max_attempts:
                updated = task.model_copy(
                    update={
                        "state": TaskState.BLOCKED,
                        "reason": (
                            "Review requested remediation, but the configured "
                            "attempt limit was exhausted."
                        ),
                        "worker_id": None,
                    }
                )
            else:
                updated = task.model_copy(
                    update={
                        "state": TaskState.READY,
                        "reason": (f"Human review requested changes: {rationale}"),
                        "worker_id": None,
                    }
                )
            changes.append(updated)
        return tuple(changes)
