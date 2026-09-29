from datetime import UTC, datetime, timedelta
from pathlib import Path
from threading import Lock
from time import sleep

import pytest

from cronos_ai.approval import plan_fingerprint
from cronos_ai.attention import HumanActionProcessor
from cronos_ai.models import (
    ApprovalDecision,
    Attempt,
    AttemptStatus,
    ControlAction,
    ControlActionType,
    OpenSpecPlan,
    PlanApproval,
    PlanTask,
    PlanTaskKind,
    RunContext,
    TaskRecord,
    TaskState,
    TriageOutcome,
    WorkerSlot,
    WorkerStatus,
    WorkRequest,
)
from cronos_ai.scheduler import (
    BoundedTaskScheduler,
    ExecutionGraph,
    SchedulerError,
    TransientWorkerError,
)
from cronos_ai.storage import FactoryStore
from cronos_ai.triage import triage_request


def make_run() -> tuple[WorkRequest, OpenSpecPlan]:
    request = WorkRequest(
        request_id="request-1",
        description="Implement and verify a feature",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="implement-feature",
        tasks=(
            PlanTask(task_id="implement", description="Implement the feature."),
            PlanTask(
                task_id="verify",
                description="Verify the feature.",
                depends_on=("implement",),
            ),
        ),
    )
    return request, plan


def test_bounded_scheduler_reserves_only_ready_tasks_and_reuses_slots(
    tmp_path,
) -> None:
    request = WorkRequest(
        request_id="request-scheduler",
        description="Schedule independent and dependent tasks",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="schedule-work",
        tasks=(
            PlanTask(task_id="first", description="First task"),
            PlanTask(
                task_id="dependent",
                description="Wait for first task",
                depends_on=("first",),
            ),
            PlanTask(task_id="parallel", description="Parallel task"),
            PlanTask(task_id="next", description="Next independent task"),
        ),
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-scheduler", request, plan)
        scheduler = BoundedTaskScheduler(
            "run-scheduler",
            store,
            graph,
            worker_ids=("worker-1", "worker-2", "worker-3"),
            max_concurrency=2,
        )

        assignments = scheduler.dispatch_ready()

        assert len(assignments) == 2
        assert {item.task_id for item in assignments} == {"first", "parallel"}
        assert len({item.worker_id for item in assignments}) == 2
        assert store.get_task("run-scheduler", "dependent").state is TaskState.QUEUED
        assert store.get_task("run-scheduler", "next").state is TaskState.READY
        assert scheduler.dispatch_ready() == []

        first = store.get_task("run-scheduler", "first")
        assert first is not None
        store.save_task(
            "run-scheduler",
            first.model_copy(update={"state": TaskState.REVIEW, "worker_id": None}),
        )
        next_wave = scheduler.dispatch_ready()

        assert len(next_wave) == 1
        assert next_wave[0].task_id == "dependent"
        assert next_wave[0].worker_id == "worker-1"
        assert store.get_task("run-scheduler", "dependent").state is TaskState.RUNNING
        assert store.get_task("run-scheduler", "next").state is TaskState.READY


def test_scheduler_runs_a_bounded_batch_and_reuses_worker_slots(tmp_path) -> None:
    request = WorkRequest(
        request_id="request-parallel",
        description="Run independent work in parallel",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="parallel-work",
        tasks=tuple(
            PlanTask(task_id=f"task-{index}", description=f"Task {index}")
            for index in range(4)
        ),
    )
    active = 0
    maximum_active = 0
    lock = Lock()
    dispatched: list[tuple[str, str]] = []

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-parallel", request, plan)
        scheduler = BoundedTaskScheduler(
            "run-parallel",
            store,
            graph,
            worker_ids=("worker-1", "worker-2", "worker-3"),
            max_concurrency=2,
        )

        def dispatch(assignment) -> None:
            nonlocal active, maximum_active
            with lock:
                active += 1
                maximum_active = max(maximum_active, active)
                dispatched.append((assignment.task_id, assignment.worker_id))
            # Exercise concurrent access to the shared SQLite execution ledger.
            record = store.get_task("run-parallel", assignment.task_id)
            assert record is not None and record.state is TaskState.RUNNING
            slot = WorkerSlot(
                worker_id=assignment.worker_id,
                session_id="scheduler-test",
                workspace_id=f"w-{assignment.worker_id}",
                pane_id=f"w-{assignment.worker_id}:p1",
                status=WorkerStatus.WORKING,
                active_task_id=assignment.task_id,
            )
            store.save_worker_slot(slot)
            sleep(0.03)
            store.save_worker_slot(
                slot.model_copy(
                    update={
                        "status": WorkerStatus.DONE,
                        "active_task_id": None,
                    }
                )
            )
            with lock:
                active -= 1

        results = scheduler.run_ready(dispatch)

        assert len(results) == 4
        assert maximum_active == 2
        assert {worker_id for _task_id, worker_id in dispatched} == {
            "worker-1",
            "worker-2",
        }
        assert all(result.success for result in results)
        assert all(
            task.state is TaskState.INTEGRATING
            for task in store.list_tasks("run-parallel")
        )


def test_scheduler_marks_dispatch_failures_with_reason(tmp_path) -> None:
    request = WorkRequest(
        request_id="request-failure",
        description="Run failing worker",
        repo_path="/workspace/project",
    )
    plan = OpenSpecPlan(
        change_name="failing-worker",
        tasks=(PlanTask(task_id="task-1", description="Fails"),),
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-failure", request, plan)
        scheduler = BoundedTaskScheduler(
            "run-failure",
            store,
            graph,
            worker_ids=("worker-1",),
            max_concurrency=1,
        )

        def fail(_assignment) -> None:
            raise RuntimeError("fake worker crashed")

        results = scheduler.run_ready(fail)
        failed = store.get_task("run-failure", "task-1")

        assert len(results) == 1
        assert results[0].success is False
        assert failed is not None
        assert failed.state is TaskState.FAILED
        assert failed.reason == "fake worker crashed"
        assert failed.worker_id is None


def test_transient_worker_failures_retry_only_to_configured_attempt_limit(
    tmp_path,
) -> None:
    request, plan = make_run()
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-1", request, plan)
        scheduler = BoundedTaskScheduler(
            "run-1",
            store,
            graph,
            worker_ids=("worker-1",),
            max_concurrency=1,
            max_attempts=2,
        )
        attempts: list[int] = []

        def fail_transiently(assignment) -> None:
            attempts.append(assignment.attempt_number)
            raise TransientWorkerError("temporary worker interruption")

        outcomes = scheduler.run_ready(fail_transiently)
        task = store.get_task("run-1", "implement")
        attempt_history = store.get_attempts("run-1", "implement")

        assert attempts == [1, 2]
        assert len(outcomes) == 2
        assert all(not outcome.success for outcome in outcomes)
        assert task.state is TaskState.FAILED
        assert task.reason == "temporary worker interruption"
        assert task.attempt_count == 2
        assert [attempt.attempt_number for attempt in attempt_history] == [1, 2]
        assert all(
            attempt.status is AttemptStatus.FAILED for attempt in attempt_history
        )
        assert all(attempt.transient for attempt in attempt_history)
        assert scheduler.run_ready(fail_transiently) == []


def test_transient_retry_can_succeed_and_preserves_attempt_history(tmp_path) -> None:
    request, plan = make_run()
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-1", request, plan)
        scheduler = BoundedTaskScheduler(
            "run-1",
            store,
            graph,
            worker_ids=("worker-1",),
            max_concurrency=1,
            max_attempts=3,
        )
        attempts: list[int] = []

        def fail_once_then_settle(assignment) -> None:
            attempts.append(assignment.attempt_number)
            if assignment.attempt_number == 1:
                raise TransientWorkerError("retryable timeout")

        outcomes = scheduler.run_ready(fail_once_then_settle)
        task = store.get_task("run-1", "implement")
        history = store.get_attempts("run-1", "implement")

        assert attempts == [1, 2]
        assert [outcome.success for outcome in outcomes] == [False, True]
        assert task.state is TaskState.INTEGRATING
        assert task.attempt_count == 2
        assert [attempt.status for attempt in history] == [
            AttemptStatus.FAILED,
            AttemptStatus.SUCCEEDED,
        ]


def test_dependency_ready_tasks_follow_the_persisted_graph(tmp_path) -> None:
    request, plan = make_run()
    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-1", request, plan)

        assert graph.refresh("run-1") == ["implement"]
        assert store.get_task("run-1", "implement").state is TaskState.READY
        assert store.get_task("run-1", "verify").state is TaskState.QUEUED

        store.record_attempt(
            "run-1",
            TaskRecord(
                task_id="implement",
                state=TaskState.DONE,
                attempt_count=1,
            ),
            Attempt(
                task_id="implement",
                attempt_number=1,
                status=AttemptStatus.SUCCEEDED,
                started_at=datetime.now(UTC),
                finished_at=datetime.now(UTC) + timedelta(seconds=1),
            ),
        )

        assert graph.refresh("run-1") == ["verify"]
        assert store.get_task("run-1", "verify").state is TaskState.READY


def test_task_state_and_attempt_history_survive_graph_recreation(tmp_path) -> None:
    database_path = tmp_path / "factory.sqlite3"
    request, plan = make_run()
    finished_attempt = Attempt(
        task_id="implement",
        attempt_number=1,
        status=AttemptStatus.SUCCEEDED,
        started_at=datetime.now(UTC),
        finished_at=datetime.now(UTC) + timedelta(seconds=1),
    )

    with FactoryStore(database_path) as store:
        graph = ExecutionGraph(store)
        graph.initialize_run("run-1", request, plan)
        graph.refresh("run-1")
        store.record_attempt(
            "run-1",
            TaskRecord(
                task_id="implement",
                state=TaskState.DONE,
                attempt_count=1,
            ),
            finished_attempt,
        )

    with FactoryStore(database_path) as recovered_store:
        recovered_graph = ExecutionGraph(recovered_store)
        assert recovered_graph.refresh("run-1") == ["verify"]
        assert recovered_store.get_task("run-1", "implement") == TaskRecord(
            task_id="implement",
            state=TaskState.DONE,
            attempt_count=1,
        )
        assert recovered_store.get_task("run-1", "verify").state is TaskState.READY
        assert recovered_store.get_attempts("run-1", "implement") == [finished_attempt]


def test_specs_required_plan_is_not_dispatched_without_matching_approval(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-approval",
        description="Implement a specifications-required change",
        repo_path="/workspace/project",
        triage_outcome=TriageOutcome.SPECS_REQUIRED,
    )
    plan = OpenSpecPlan(
        change_name="approved-plan",
        tasks=(PlanTask(task_id="implement", description="Implement"),),
    )
    run_worktree = tmp_path / "run-worktree"
    change_dir = run_worktree / "openspec" / "changes" / plan.change_name
    change_dir.mkdir(parents=True)
    (change_dir / "proposal.md").write_text("Approved plan contents\n")
    plan_hash = plan_fingerprint(change_dir)
    context = RunContext(
        run_id="run-approval",
        branch_name="factory/run/approval",
        run_worktree_path=run_worktree,
        plan_hash=plan_hash,
        approval_required=True,
    )

    triage = triage_request(
        request,
        TriageOutcome.SPECS_REQUIRED,
        rationale="A detailed specification and approval are required.",
    )
    request = triage.request

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        with pytest.raises(SchedulerError, match="approval context"):
            graph.initialize_run("run-approval", request, plan, triage_result=triage)

        graph.initialize_run(
            "run-approval",
            request,
            plan,
            run_context=context,
            triage_result=triage,
        )
        scheduler = BoundedTaskScheduler(
            "run-approval",
            store,
            graph,
            worker_ids=("worker-1",),
            max_concurrency=1,
        )
        assert scheduler.dispatch_ready() == []
        assert store.get_task("run-approval", "implement").state is TaskState.QUEUED

        store.record_plan_approval(
            "run-approval",
            PlanApproval(
                change_name=plan.change_name,
                plan_hash=plan_hash,
                decision=ApprovalDecision.APPROVED,
                reviewer="operator-1",
            ),
        )
        assignments = scheduler.dispatch_ready()

    assert [assignment.task_id for assignment in assignments] == ["implement"]


def test_plan_and_approval_tasks_are_not_sent_to_implementation_workers(
    tmp_path: Path,
) -> None:
    request = WorkRequest(
        request_id="request-phases",
        description="Implement a detailed feature",
        repo_path="/workspace/project",
    )
    triage = triage_request(
        request,
        TriageOutcome.SPECS_REQUIRED,
        rationale="The feature needs a detailed plan and human approval.",
    )
    request = triage.request
    plan = OpenSpecPlan(
        change_name="phased-plan",
        tasks=(
            PlanTask(
                task_id="plan-design",
                description="Record the detailed design",
                kind=PlanTaskKind.PLANNING,
            ),
            PlanTask(
                task_id="plan-approval",
                description="Obtain human approval",
                depends_on=("plan-design",),
                kind=PlanTaskKind.APPROVAL,
            ),
            PlanTask(
                task_id="implement",
                description="Implement approved work",
                depends_on=("plan-approval",),
                kind=PlanTaskKind.IMPLEMENTATION,
            ),
        ),
    )
    run_worktree = tmp_path / "run-worktree"
    change_dir = run_worktree / "openspec" / "changes" / plan.change_name
    change_dir.mkdir(parents=True)
    (change_dir / "proposal.md").write_text("Detailed plan\n")
    plan_hash = plan_fingerprint(change_dir)
    context = RunContext(
        run_id="run-phases",
        branch_name="factory/run/phases",
        run_worktree_path=run_worktree,
        plan_hash=plan_hash,
        approval_required=True,
    )

    with FactoryStore(tmp_path / "factory.sqlite3") as store:
        graph = ExecutionGraph(store)
        graph.initialize_run(
            "run-phases",
            request,
            plan,
            run_context=context,
            triage_result=triage,
        )
        records = {task.task_id: task for task in store.list_tasks("run-phases")}
        assert records["plan-design"].state is TaskState.DONE
        assert records["plan-approval"].state is TaskState.WAITING_FOR_HUMAN
        assert records["implement"].state is TaskState.QUEUED
        scheduler = BoundedTaskScheduler(
            "run-phases",
            store,
            graph,
            worker_ids=("worker-1",),
            max_concurrency=1,
        )
        assert scheduler.dispatch_ready() == []
        action = ControlAction(
            action_id="approve-phased-plan",
            action_type=ControlActionType.APPROVE_PLAN,
            target_id=plan.change_name,
            run_id="run-phases",
            payload={"plan_hash": plan_hash, "reviewer": "operator-1"},
        )
        store.enqueue_control_action(action)
        result = HumanActionProcessor(store).process_pending()[0]
        assert result.status.value == "done"
        assert store.get_task("run-phases", "plan-approval").state is TaskState.DONE
        assignments = scheduler.dispatch_ready()

    assert [assignment.task_id for assignment in assignments] == ["implement"]
