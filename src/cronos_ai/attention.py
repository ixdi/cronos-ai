"""Human-facing attention queue derived from durable run state."""

from __future__ import annotations

from cronos_ai.approval import ApprovalError, plan_fingerprint
from cronos_ai.models import (
    ApprovalDecision,
    AttentionItem,
    ControlAction,
    ControlActionRecord,
    ControlActionStatus,
    ControlActionType,
    DeliveryStatus,
    PlanApproval,
    PlanTaskKind,
    RunContext,
    TaskState,
    TriageOutcome,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore, StorageError


class HumanAttentionQueue:
    """Find work that needs an explicit human decision, not dependency waits."""

    def __init__(self, store: FactoryStore, *, max_attempts: int = 3) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        self.store = store
        self.max_attempts = max_attempts

    def list_items(self, run_id: str | None = None) -> list[AttentionItem]:
        items: list[AttentionItem] = []
        run_ids = [run_id] if run_id is not None else self.store.list_run_ids()

        for current_run_id in run_ids:
            run = self.store.get_run(current_run_id)
            if run is None:
                continue
            _, plan = run
            context = self.store.get_run_context(current_run_id)
            if context is not None and context.approval_required:
                current_hash = context.plan_hash
                change_dir = (
                    context.run_worktree_path
                    / "openspec"
                    / "changes"
                    / plan.change_name
                )
                try:
                    current_hash = plan_fingerprint(change_dir)
                except ApprovalError:
                    pass
                approval = (
                    self.store.get_plan_approval(
                        current_run_id, plan.change_name, current_hash
                    )
                    if current_hash is not None
                    else None
                )
                if (
                    approval is None
                    or approval.decision is not ApprovalDecision.APPROVED
                ):
                    reason = (
                        "Plan approval is required before implementation can dispatch."
                    )
                    if context.plan_hash != current_hash:
                        reason = "The plan changed; approve its current fingerprint."
                    if approval is not None:
                        reason = approval.rationale or "The plan was not approved."
                    items.append(
                        AttentionItem(
                            item_id=f"{current_run_id}:plan-approval",
                            run_id=current_run_id,
                            target_id=plan.change_name,
                            state=TaskState.WAITING_FOR_HUMAN,
                            reason=reason,
                            latest_result=(
                                f"Current plan fingerprint: {current_hash}"
                                if current_hash is not None
                                else "The current plan fingerprint is unavailable."
                            ),
                            suggested_data={
                                "plan_hash": current_hash or "",
                                "reviewer": "local-operator",
                            },
                            available_actions=(ControlActionType.APPROVE_PLAN,),
                        )
                    )

            review_packet = self.store.get_latest_review_packet(current_run_id)
            if review_packet is not None and review_packet.state.value == "ready":
                items.append(
                    AttentionItem(
                        item_id=f"{current_run_id}:review",
                        run_id=current_run_id,
                        target_id="run",
                        state=TaskState.REVIEW,
                        reason=review_packet.review_summary,
                        latest_result=(
                            f"Review fingerprint: {review_packet.review_hash}; "
                            f"diff fingerprint: {review_packet.diff_hash}"
                        ),
                        suggested_data={
                            "review_hash": review_packet.review_hash,
                            "reviewer": "local-operator",
                            "rationale": "DESCRIBE_REQUESTED_CHANGES",
                        },
                        available_actions=(
                            ControlActionType.APPROVE_REVIEW,
                            ControlActionType.REJECT_REVIEW,
                        ),
                    )
                )

            if review_packet is not None and review_packet.state.value == "approved":
                delivery = self.store.get_delivery_record(
                    current_run_id, review_packet.review_hash
                )
                if delivery is not None and delivery.status in (
                    DeliveryStatus.FAILED,
                    DeliveryStatus.INCONCLUSIVE,
                ):
                    items.append(
                        AttentionItem(
                            item_id=f"{current_run_id}:delivery",
                            run_id=current_run_id,
                            target_id="run",
                            state=TaskState.BLOCKED,
                            reason=(
                                "CI/CD delivery did not complete successfully: "
                                f"{delivery.summary}"
                            ),
                            latest_result=(
                                f"{delivery.status.value}; workflow "
                                f"{delivery.external_id or 'ID unavailable'}"
                            ),
                            recommended_action=(
                                "Inspect the CI/CD workflow, remediate its failure, "
                                "and require a fresh approved review before delivery."
                            ),
                        )
                    )

            plan_approval_task_ids = {
                task.task_id
                for task in plan.tasks
                if task.kind is PlanTaskKind.APPROVAL
            }
            for task in self.store.list_tasks(current_run_id):
                if (
                    task.task_id in plan_approval_task_ids
                    and context is not None
                    and context.approval_required
                ):
                    continue
                actions = self._task_actions(
                    task.state,
                    task.reason,
                    task.attempt_count,
                    self.max_attempts,
                )
                recommendation = None
                if not actions:
                    if (
                        task.state is TaskState.FAILED
                        and task.attempt_count >= self.max_attempts
                    ):
                        recommendation = (
                            "The configured attempt limit is exhausted. "
                            "Review the failure and update policy before resuming."
                        )
                    elif task.state is TaskState.BLOCKED and (
                        task.reason is not None
                        and "attempt limit" in task.reason.casefold()
                    ):
                        recommendation = (
                            "The remediation attempt limit is exhausted. "
                            "Review the recovery policy before resuming."
                        )
                    else:
                        continue
                target_id = task.task_id
                suggested_data = self._suggested_data(actions)
                if actions == (ControlActionType.APPROVE_PLAN,):
                    if context is None or not context.approval_required:
                        actions = (ControlActionType.CLARIFY_REQUEST,)
                        suggested_data = self._suggested_data(actions)
                    else:
                        target_id = plan.change_name
                        current_hash = context.plan_hash or ""
                        try:
                            current_hash = plan_fingerprint(
                                context.run_worktree_path
                                / "openspec"
                                / "changes"
                                / plan.change_name
                            )
                        except ApprovalError:
                            pass
                        suggested_data = {
                            "plan_hash": current_hash,
                            "reviewer": "local-operator",
                        }
                reason = task.reason or self._default_task_reason(task.state)
                attempts = self.store.get_attempts(current_run_id, task.task_id)
                latest_result = None
                if attempts:
                    latest = attempts[-1]
                    latest_result = (
                        f"Attempt {latest.attempt_number}: {latest.status.value}"
                    )
                    if latest.reason:
                        latest_result += f" - {latest.reason}"
                items.append(
                    AttentionItem(
                        item_id=f"{current_run_id}:{task.task_id}",
                        run_id=current_run_id,
                        target_id=target_id,
                        task_id=task.task_id,
                        state=task.state,
                        reason=reason,
                        latest_result=latest_result,
                        suggested_data=suggested_data,
                        available_actions=actions,
                        recommended_action=recommendation,
                    )
                )

        for request in self.store.list_requests():
            if request.triage_outcome is TriageOutcome.CLARIFICATION_REQUIRED:
                items.append(
                    AttentionItem(
                        item_id=f"request:{request.request_id}",
                        target_id=request.request_id,
                        request_id=request.request_id,
                        state=TriageOutcome.CLARIFICATION_REQUIRED,
                        reason=(
                            "The request needs clarification before planning can "
                            "continue."
                        ),
                        latest_result=request.description,
                        suggested_data={"answer": "REPLACE_WITH_CLARIFICATION"},
                        available_actions=(ControlActionType.CLARIFY_REQUEST,),
                    )
                )
            elif request.triage_outcome is TriageOutcome.PARKED:
                items.append(
                    AttentionItem(
                        item_id=f"request:{request.request_id}",
                        target_id=request.request_id,
                        request_id=request.request_id,
                        state=TriageOutcome.PARKED,
                        reason=(
                            "The request is parked and requires an explicit resume "
                            "decision."
                        ),
                        latest_result=request.description,
                        available_actions=(ControlActionType.RESUME_REQUEST,),
                    )
                )

        return sorted(items, key=lambda item: item.item_id)

    @staticmethod
    def _suggested_data(
        actions: tuple[ControlActionType, ...],
    ) -> dict[str, str]:
        if actions == (ControlActionType.CLARIFY_REQUEST,):
            return {"answer": "REPLACE_WITH_CLARIFICATION"}
        if actions == (ControlActionType.RESOLVE_CONFLICT,):
            return {"resolution": "DESCRIBE_RESOLUTION"}
        if actions == (
            ControlActionType.APPROVE_REVIEW,
            ControlActionType.REJECT_REVIEW,
        ):
            return {
                "reviewer": "local-operator",
                "rationale": "DESCRIBE_REQUESTED_CHANGES",
            }
        return {}

    @staticmethod
    def _task_actions(
        state: TaskState,
        reason: str | None,
        attempt_count: int,
        max_attempts: int,
    ) -> tuple[ControlActionType, ...]:
        normalized_reason = (reason or "").casefold()
        if state is TaskState.FAILED:
            return (
                (ControlActionType.RETRY_TASK,) if attempt_count < max_attempts else ()
            )
        if state is TaskState.BLOCKED:
            if "conflict" in normalized_reason:
                return (ControlActionType.RESOLVE_CONFLICT,)
            return (ControlActionType.CLARIFY_REQUEST,)
        if state is TaskState.WAITING_FOR_HUMAN:
            if "approval" in normalized_reason or "approve" in normalized_reason:
                return (ControlActionType.APPROVE_PLAN,)
            return (ControlActionType.CLARIFY_REQUEST,)
        return ()

    @staticmethod
    def _default_task_reason(state: TaskState) -> str:
        return {
            TaskState.FAILED: "The task failed and needs a recovery decision.",
            TaskState.BLOCKED: "The task is blocked and needs human intervention.",
            TaskState.WAITING_FOR_HUMAN: "The task is waiting for a human decision.",
            TaskState.REVIEW: "The integrated task is ready for human review.",
        }.get(state, "The task needs human attention.")


class HumanActionProcessor:
    """Apply queued human actions and retain their processing audit outcome."""

    def __init__(self, store: FactoryStore, *, max_attempts: int = 3) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        self.store = store
        self.max_attempts = max_attempts

    def process_pending(self) -> list[ControlActionRecord]:
        return [
            self.process_action(action.action_id)
            for action in self.store.list_pending_actions()
        ]

    def recover_interrupted(self) -> int:
        """Make in-flight actions retryable after a controller restart."""
        return self.store.requeue_interrupted_actions()

    def process_action(self, action_id: str) -> ControlActionRecord:
        record = self.store.get_control_action_record(action_id)
        if record is None:
            raise StorageError(f"control action does not exist: {action_id}")
        if record.status is not ControlActionStatus.PENDING:
            raise StorageError(f"control action is not pending: {action_id}")
        action = record.action
        self.store.set_control_action_status(action_id, ControlActionStatus.PROCESSING)
        try:
            self._apply(action)
        except Exception as error:
            message = str(error).strip() or type(error).__name__
            return self.store.set_control_action_status(
                action_id,
                ControlActionStatus.FAILED,
                message[:1000],
            )
        return self.store.set_control_action_status(action_id, ControlActionStatus.DONE)

    def _apply(self, action: ControlAction) -> None:
        if action.action_type is ControlActionType.CLARIFY_REQUEST:
            self._clarify(action)
        elif action.action_type is ControlActionType.RESUME_REQUEST:
            self._resume_request(action)
        elif action.action_type is ControlActionType.RETRY_TASK:
            self._retry_task(action)
        elif action.action_type is ControlActionType.APPROVE_PLAN:
            self._approve_plan(action)
        elif action.action_type in (
            ControlActionType.APPROVE_REVIEW,
            ControlActionType.REJECT_REVIEW,
        ):
            self._review(action)
        elif action.action_type is ControlActionType.RESOLVE_CONFLICT:
            self._resolve_conflict(action)
        else:
            raise ValueError(f"unsupported human action: {action.action_type.value}")

    def _clarify(self, action: ControlAction) -> None:
        answer = action.payload["answer"]
        if action.run_id is not None:
            task = self.store.get_task(action.run_id, action.target_id)
            if (
                task is not None
                and task.state is TaskState.READY
                and task.reason is not None
                and action.action_id in task.reason
            ):
                return
            if task is None or task.state not in (
                TaskState.WAITING_FOR_HUMAN,
                TaskState.BLOCKED,
            ):
                raise ValueError("target task is not waiting for human input")
            self.store.save_task(
                action.run_id,
                task.model_copy(
                    update={
                        "state": TaskState.READY,
                        "reason": (
                            f"Human clarification ({action.action_id}): {answer}"
                        ),
                        "worker_id": None,
                    }
                ),
            )
            return
        request = self._find_request(action.target_id)
        marker = f"[human-action:{action.action_id}]"
        if marker in request.description:
            return
        updated = request.model_copy(
            update={
                "description": (
                    f"{request.description}\n\n{marker}\nHuman clarification: {answer}"
                ),
                "triage_outcome": None,
            }
        )
        self.store.update_request(updated)

    def _resume_request(self, action: ControlAction) -> None:
        request = self._find_request(action.target_id)
        self.store.update_request(request.model_copy(update={"triage_outcome": None}))

    def _retry_task(self, action: ControlAction) -> None:
        assert action.run_id is not None
        task = self.store.get_task(action.run_id, action.target_id)
        if task is None:
            raise ValueError("target task does not exist in the specified run")
        if (
            task.state is TaskState.READY
            and task.reason
            and action.action_id in task.reason
        ):
            return
        if task.state is not TaskState.FAILED:
            raise ValueError("only failed tasks can be retried")
        if task.attempt_count >= self.max_attempts:
            raise ValueError(
                f"task has reached the configured attempt limit ({self.max_attempts})"
            )
        self.store.save_task(
            action.run_id,
            task.model_copy(
                update={
                    "state": TaskState.READY,
                    "reason": (
                        f"Human retry approved by {action.actor} ({action.action_id})."
                    ),
                    "worker_id": None,
                }
            ),
        )

    def _approve_plan(self, action: ControlAction) -> None:
        assert action.run_id is not None
        context = self._require_run_context(action.run_id)
        run = self.store.get_run(action.run_id)
        if run is None:
            raise ValueError("target run does not exist")
        _, plan = run
        if action.target_id != plan.change_name:
            raise ValueError("approval target does not match the run plan")
        requested_hash = action.payload["plan_hash"]
        try:
            current_hash = plan_fingerprint(
                context.run_worktree_path / "openspec" / "changes" / plan.change_name
            )
        except ApprovalError as error:
            raise ValueError("current OpenSpec plan cannot be fingerprinted") from error
        if requested_hash != current_hash:
            raise ValueError("approval is stale or does not match the current plan")
        if context.plan_hash != current_hash:
            context = context.model_copy(update={"plan_hash": current_hash})
            self.store.save_run_context(context)
        prior = self.store.get_plan_approval(
            action.run_id, plan.change_name, current_hash
        )
        if not (
            prior is not None
            and prior.decision is ApprovalDecision.APPROVED
            and prior.reviewer == action.payload["reviewer"]
        ):
            self.store.record_plan_approval(
                action.run_id,
                PlanApproval(
                    change_name=plan.change_name,
                    plan_hash=requested_hash,
                    decision=ApprovalDecision.APPROVED,
                    reviewer=action.payload["reviewer"],
                    rationale=action.payload.get("rationale"),
                ),
            )
        for plan_task in plan.tasks:
            if plan_task.kind is not PlanTaskKind.APPROVAL:
                continue
            task = self.store.get_task(action.run_id, plan_task.task_id)
            if task is not None and task.state is TaskState.WAITING_FOR_HUMAN:
                self.store.save_task(
                    action.run_id,
                    task.model_copy(
                        update={
                            "state": TaskState.DONE,
                            "reason": None,
                            "worker_id": None,
                        }
                    ),
                )

    def _review(self, action: ControlAction) -> None:
        assert action.run_id is not None
        if action.target_id == "run":
            from cronos_ai.review import ReviewCoordinator

            ReviewCoordinator(self.store, max_attempts=self.max_attempts).decide_review(
                action.run_id,
                review_hash=action.payload["review_hash"],
                reviewer=action.payload["reviewer"],
                approve=action.action_type is ControlActionType.APPROVE_REVIEW,
                rationale=action.payload.get("rationale"),
            )
            return
        tasks = self.store.list_tasks(action.run_id)
        scope = [task for task in tasks if action.target_id in ("run", task.task_id)]
        selected = [task for task in scope if task.state is TaskState.REVIEW]
        if not selected:
            already_applied = (
                all(task.state is TaskState.DONE for task in scope)
                if action.action_type is ControlActionType.APPROVE_REVIEW
                else all(
                    task.state is TaskState.READY
                    and task.reason is not None
                    and action.action_id in task.reason
                    for task in scope
                )
            )
            if scope and already_applied:
                return
            raise ValueError("no task in the target scope is awaiting review")
        for task in selected:
            if action.action_type is ControlActionType.APPROVE_REVIEW:
                updated = task.model_copy(
                    update={"state": TaskState.DONE, "reason": None, "worker_id": None}
                )
            else:
                rationale = action.payload.get("rationale", "Changes requested.")
                updated = task.model_copy(
                    update={
                        "state": TaskState.READY,
                        "reason": (
                            f"Human review requested changes: {rationale} "
                            f"({action.action_id})"
                        ),
                        "worker_id": None,
                    }
                )
            self.store.save_task(action.run_id, updated)

    def _resolve_conflict(self, action: ControlAction) -> None:
        assert action.run_id is not None
        context = self._require_run_context(action.run_id)
        from cronos_ai.task_integration import TaskIntegrator

        integrator = TaskIntegrator(
            action.run_id,
            context.branch_name,
            context.run_worktree_path,
            self.store,
            starting_commit=context.starting_commit,
            task_worktree_root=context.task_worktree_root,
        )
        integrator.resolve_conflict(action.target_id, action.payload["resolution"])

    def _find_request(self, request_id: str) -> WorkRequest:
        for request in self.store.list_requests():
            if request.request_id == request_id:
                return request
        raise ValueError(f"target request does not exist: {request_id}")

    def _require_run_context(self, run_id: str) -> RunContext:
        context = self.store.get_run_context(run_id)
        if context is None:
            raise ValueError(f"run context is not registered: {run_id}")
        return context
