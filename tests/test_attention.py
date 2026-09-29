from datetime import UTC, datetime, timedelta
from pathlib import Path
from uuid import uuid4

from cronos_ai.approval import plan_fingerprint
from cronos_ai.attention import HumanActionProcessor, HumanAttentionQueue
from cronos_ai.models import (
    ApprovalDecision,
    Attempt,
    AttemptStatus,
    ControlAction,
    ControlActionStatus,
    ControlActionType,
    OpenSpecPlan,
    PlanApproval,
    PlanTask,
    RunContext,
    TaskRecord,
    TaskState,
    TriageOutcome,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore


def test_attention_queue_excludes_dependency_waits_and_lists_next_actions(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-1",
        description="Clarify expected behavior",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.CLARIFICATION_REQUIRED,
    )
    plan = OpenSpecPlan(
        change_name="attention-test",
        tasks=(
            PlanTask(task_id="dependency", description="Waiting on another task"),
            PlanTask(
                task_id="question",
                description="Needs a human decision",
                depends_on=("dependency",),
            ),
            PlanTask(task_id="conflict", description="Merge task branch"),
            PlanTask(task_id="failed", description="Worker stopped"),
            PlanTask(task_id="review", description="Review integrated change"),
        ),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run(
            "run-1",
            request,
            plan,
            (
                TaskRecord(task_id="dependency", state=TaskState.QUEUED),
                TaskRecord(
                    task_id="question",
                    state=TaskState.WAITING_FOR_HUMAN,
                    reason="Clarification is required.",
                ),
                TaskRecord(
                    task_id="conflict",
                    state=TaskState.BLOCKED,
                    reason="Merge conflict requires human resolution: README.md",
                ),
                TaskRecord(
                    task_id="failed",
                    state=TaskState.FAILED,
                    reason="Transient retries exhausted.",
                    attempt_count=1,
                ),
                TaskRecord(task_id="review", state=TaskState.REVIEW),
            ),
        )
        started = datetime.now(UTC)
        store.record_attempt(
            "run-1",
            TaskRecord(
                task_id="failed",
                state=TaskState.FAILED,
                reason="Transient retries exhausted.",
                attempt_count=1,
            ),
            Attempt(
                task_id="failed",
                attempt_number=1,
                status=AttemptStatus.FAILED,
                started_at=started,
                finished_at=started + timedelta(seconds=1),
                reason="worker process exited",
                transient=True,
            ),
        )
        store.enqueue_request(request)

        items = HumanAttentionQueue(store).list_items()

    by_id = {item.item_id: item for item in items}
    assert "run-1:dependency" not in by_id
    assert set(by_id) == {
        "run-1:question",
        "run-1:conflict",
        "run-1:failed",
        "request:request-1",
    }
    assert by_id["run-1:question"].available_actions == (
        ControlActionType.CLARIFY_REQUEST,
    )
    assert by_id["run-1:conflict"].available_actions == (
        ControlActionType.RESOLVE_CONFLICT,
    )
    assert by_id["run-1:failed"].available_actions == (ControlActionType.RETRY_TASK,)
    assert "worker process exited" in by_id["run-1:failed"].latest_result
    assert by_id["request:request-1"].available_actions == (
        ControlActionType.CLARIFY_REQUEST,
    )


def test_human_retry_is_applied_and_recorded_against_run_and_task(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-1",
        description="Retry this task",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="retry-test",
        tasks=(PlanTask(task_id="failed", description="Recover worker output"),),
    )
    failed = TaskRecord(
        task_id="failed",
        state=TaskState.FAILED,
        reason="Worker exited unexpectedly.",
        attempt_count=1,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.RETRY_TASK,
        target_id="failed",
        run_id="run-1",
        actor="operator-1",
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-1", request, plan, (failed,))
        store.enqueue_control_action(action)
        results = HumanActionProcessor(store, max_attempts=2).process_pending()
        current = store.get_task("run-1", "failed")
        audit = store.get_control_action_record(action.action_id)

    assert current is not None
    assert current.state is TaskState.READY
    assert audit is not None
    assert audit.action.actor == "operator-1"
    assert audit.action.run_id == "run-1"
    assert audit.action.target_id == "failed"
    assert audit.status.value == "done"
    assert results[0].status.value == "done"


def test_retry_beyond_configured_attempt_limit_is_audited_as_failed(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-1",
        description="Do not exceed retry policy",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="retry-limit",
        tasks=(PlanTask(task_id="failed", description="Exhausted task"),),
    )
    failed = TaskRecord(
        task_id="failed",
        state=TaskState.FAILED,
        reason="Retry limit exhausted.",
        attempt_count=2,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.RETRY_TASK,
        target_id="failed",
        run_id="run-1",
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-1", request, plan, (failed,))
        store.enqueue_control_action(action)
        result = HumanActionProcessor(store, max_attempts=2).process_pending()[0]
        current = store.get_task("run-1", "failed")

    assert result.status.value == "failed"
    assert "attempt limit" in (result.error or "")
    assert current is not None and current.state is TaskState.FAILED


def test_exhausted_failure_shows_policy_escalation_without_retry_action(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-exhausted",
        description="Do not exceed retry policy",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="exhausted-retry",
        tasks=(PlanTask(task_id="failed", description="Exhausted task"),),
    )
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run(
            "run-exhausted",
            request,
            plan,
            (
                TaskRecord(
                    task_id="failed",
                    state=TaskState.FAILED,
                    reason="Configured attempt limit exhausted.",
                    attempt_count=3,
                ),
            ),
        )
        item = HumanAttentionQueue(store).list_items()[0]

    assert item.item_id == "run-exhausted:failed"
    assert item.available_actions == ()
    assert "attempt limit is exhausted" in (item.recommended_action or "")


def test_clarification_action_updates_request_and_audit_record(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-clarify",
        description="Use the account default",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.CLARIFICATION_REQUIRED,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.CLARIFY_REQUEST,
        target_id="request-clarify",
        payload={"answer": "Use the current account default"},
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.enqueue_request(request)
        store.enqueue_control_action(action)
        result = HumanActionProcessor(store).process_pending()[0]
        updated = store.list_requests()[0]

    assert result.status.value == "done"
    assert updated.triage_outcome is None
    assert "Human clarification" in updated.description
    assert "Use the current account default" in updated.description


def test_plan_approval_action_is_bound_to_the_persisted_fingerprint(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-plan",
        description="Implement the approved plan",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.SPECS_REQUIRED,
    )
    plan = OpenSpecPlan(
        change_name="approved-change",
        tasks=(PlanTask(task_id="implement", description="Implement"),),
    )
    run_worktree = tmp_path / "run-plan"
    change_dir = run_worktree / "openspec" / "changes" / plan.change_name
    change_dir.mkdir(parents=True)
    (change_dir / "proposal.md").write_text("Approved plan version\n")
    plan_hash = plan_fingerprint(change_dir)
    context = RunContext(
        run_id="run-plan",
        branch_name="factory/run/plan",
        run_worktree_path=run_worktree,
        plan_hash=plan_hash,
        approval_required=True,
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.APPROVE_PLAN,
        target_id=plan.change_name,
        run_id="run-plan",
        payload={"plan_hash": plan_hash, "reviewer": "operator-1"},
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-plan", request, plan, ())
        store.save_run_context(context)
        store.enqueue_control_action(action)
        result = HumanActionProcessor(store).process_pending()[0]
        approval = store.get_plan_approval("run-plan", plan.change_name, plan_hash)

    assert result.status.value == "done"
    assert approval is not None
    assert approval.decision is ApprovalDecision.APPROVED
    assert approval.reviewer == "operator-1"


def test_plan_edit_invalidates_old_approval_and_requires_current_fingerprint(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-plan-edit",
        description="Edit a plan after review",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.SPECS_REQUIRED,
    )
    plan = OpenSpecPlan(
        change_name="editable-change",
        tasks=(PlanTask(task_id="implement", description="Implement"),),
    )
    run_worktree = tmp_path / "run-edit"
    change_dir = run_worktree / "openspec" / "changes" / plan.change_name
    change_dir.mkdir(parents=True)
    proposal = change_dir / "proposal.md"
    proposal.write_text("Version one\n")
    original_hash = plan_fingerprint(change_dir)
    proposal.write_text("Version two\n")
    current_hash = plan_fingerprint(change_dir)
    context = RunContext(
        run_id="run-edit",
        branch_name="factory/run/edit",
        run_worktree_path=run_worktree,
        plan_hash=original_hash,
        approval_required=True,
    )
    stale_action = ControlAction(
        action_id="stale-approval",
        action_type=ControlActionType.APPROVE_PLAN,
        target_id=plan.change_name,
        run_id="run-edit",
        payload={"plan_hash": original_hash, "reviewer": "operator-1"},
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run("run-edit", request, plan, ())
        store.save_run_context(context)
        store.record_plan_approval(
            "run-edit",
            PlanApproval(
                change_name=plan.change_name,
                plan_hash=original_hash,
                decision=ApprovalDecision.APPROVED,
                reviewer="operator-1",
            ),
        )
        store.enqueue_control_action(stale_action)
        attention = HumanAttentionQueue(store).list_items("run-edit")
        action_result = HumanActionProcessor(store).process_pending()[0]

    approval_item = next(
        item for item in attention if item.item_id.endswith("plan-approval")
    )
    assert approval_item.suggested_data["plan_hash"] == current_hash
    assert action_result.status is ControlActionStatus.FAILED
    assert "current plan" in (action_result.error or "")


def test_review_rejection_returns_task_to_remediation(tmp_path: Path) -> None:
    request = WorkRequest(
        request_id="request-review",
        description="Review this task",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="review-change",
        tasks=(PlanTask(task_id="review", description="Review task"),),
    )
    action = ControlAction(
        action_id=str(uuid4()),
        action_type=ControlActionType.REJECT_REVIEW,
        target_id="review",
        run_id="run-review",
        payload={"reviewer": "operator-1", "rationale": "Add missing validation"},
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run(
            "run-review",
            request,
            plan,
            (TaskRecord(task_id="review", state=TaskState.REVIEW),),
        )
        store.enqueue_control_action(action)
        result = HumanActionProcessor(store).process_pending()[0]
        current = store.get_task("run-review", "review")

    assert result.status.value == "done"
    assert current is not None
    assert current.state is TaskState.READY
    assert "Add missing validation" in (current.reason or "")


def test_interrupted_clarification_is_replayed_without_duplicate_answer(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="restart-request",
        description="Needs clarification",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.CLARIFICATION_REQUIRED,
    )
    action = ControlAction(
        action_id="restart-action",
        action_type=ControlActionType.CLARIFY_REQUEST,
        target_id=request.request_id,
        payload={"answer": "Use the configured default"},
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.enqueue_request(request)
        store.enqueue_control_action(action)
        store.set_control_action_status(
            action.action_id, ControlActionStatus.PROCESSING
        )
        processor = HumanActionProcessor(store)
        processor._apply(action)  # Simulate a crash after the request update.
        assert processor.recover_interrupted() == 1
        result = processor.process_pending()[0]
        updated = store.list_requests()[0]

    assert result.status is ControlActionStatus.DONE
    assert updated.description.count("[human-action:restart-action]") == 1


def test_attention_queue_identifies_unapproved_detailed_plan(tmp_path: Path) -> None:
    request = WorkRequest(
        request_id="request-1",
        description="Implement a substantial feature",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.SPECS_REQUIRED,
    )
    plan = OpenSpecPlan(
        change_name="substantial-feature",
        tasks=(PlanTask(task_id="implement", description="Implement the feature"),),
    )
    context = RunContext(
        run_id="run-1",
        branch_name="factory/run/request-1",
        run_worktree_path=Path("/tmp/factory-run-1"),
        starting_commit="a" * 40,
        task_worktree_root=Path("/tmp/factory-tasks/run-1"),
        plan_hash="b" * 64,
        approval_required=True,
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        store.create_run(
            "run-1",
            request,
            plan,
            (TaskRecord(task_id="implement", state=TaskState.QUEUED),),
        )
        store.save_run_context(context)

        items = HumanAttentionQueue(store).list_items()

    assert any(
        item.item_id == "run-1:plan-approval"
        and item.available_actions == (ControlActionType.APPROVE_PLAN,)
        for item in items
    )
