import hashlib
import hmac
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import pytest
from pydantic import SecretStr

from cronos_ai.attention import HumanActionProcessor
from cronos_ai.delivery import (
    DeliveryCoordinator,
    DeliveryStatus,
    FakeCIAdapter,
)
from cronos_ai.herdr import HerdrAdapter
from cronos_ai.models import (
    ControlAction,
    ControlActionType,
    RequestSource,
    SpecialistProfile,
    TaskState,
    TriageOutcome,
    UserScenarioCheck,
    VerificationCheck,
    WebhookAlertPayload,
)
from cronos_ai.recovery import RecoveryDisposition, TaskRecoveryManager
from cronos_ai.review import ReviewCoordinator
from cronos_ai.run_branch import create_run_branch_with_plan
from cronos_ai.scheduler import BoundedTaskScheduler, ExecutionGraph
from cronos_ai.storage import FactoryStore
from cronos_ai.task_integration import TaskIntegrator
from cronos_ai.triage import triage_request
from cronos_ai.webhook import WebhookEndpoint, WebhookSource
from cronos_ai.webhook_ingestion import WebhookIngestor
from cronos_ai.worker_execution import PiWorkerExecutor
from cronos_ai.worktrees import TaskWorktreeManager

WEBHOOK_SECRET = "end-to-end-test-secret-longer-than-thirty-two-bytes"
FAKE_PI_RPC = """
import json
import pathlib
import re
import subprocess
import sys

for line in sys.stdin:
    command = json.loads(line)
    if command.get("type") != "prompt":
        continue
    task_id = re.search(r"task_id=([0-9.]+)", command["message"]).group(1)
    output = pathlib.Path(f"task-{task_id}.txt")
    output.write_text(f"verified result for {task_id}\\n")
    subprocess.run(["git", "add", output.name], check=True)
    subprocess.run(
        ["git", "commit", "--quiet", "-m", "implement task"], check=True
    )
    print(json.dumps({
        "id": command["id"], "type": "response", "command": "prompt",
        "success": True,
    }), flush=True)
    print(json.dumps({
        "type": "tool_execution_start", "toolCallId": "tool-1",
        "toolName": "write", "args": {"path": output.name},
    }), flush=True)
    print(json.dumps({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "Task complete"}],
        },
    }), flush=True)
    print(json.dumps({"type": "agent_settled"}), flush=True)
"""


class FakeHerdrSocket:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.pane: dict[str, Any] = {
            "workspace_id": "e2e-workspace",
            "pane_id": "e2e-workspace:p1",
            "agent": None,
            "agent_status": "idle",
        }

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((method, params))
        if method == "ping":
            return {"type": "pong"}
        if method == "workspace.create":
            return {
                "workspace": {"workspace_id": "e2e-workspace"},
                "root_pane": self.pane,
            }
        if method == "pane.get":
            return {"pane": self.pane}
        if method == "pane.report_agent":
            self.pane["agent"] = params["agent"]
            self.pane["agent_status"] = params["state"]
            return {"accepted": True}
        if method == "pane.report_metadata":
            return {"accepted": True}
        raise AssertionError(f"unexpected Herdr method: {method}")


class FakeSandbox:
    """Host process launcher for this workflow test, not a security sandbox."""

    def __init__(self) -> None:
        self.worktrees: list[Path] = []

    def process_factory(self, worktree: Path, _profile: SpecialistProfile):
        self.worktrees.append(worktree.resolve())

        def start(command: list[str], **kwargs: Any):
            assert Path(kwargs["cwd"]).resolve() == worktree.resolve()
            return subprocess.Popen(command, **kwargs)

        return start


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
            "user.name=factory-e2e",
            "-c",
            "user.email=factory-e2e@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def test_complete_webhook_to_delivery_workflow_with_fake_adapters(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = tmp_path / "target"
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(repository, "config", "user.name", "factory-e2e")
    git(repository, "config", "user.email", "factory-e2e@example.invalid")
    (repository / "README.md").write_text("clean OpenSpec target\n")
    (repository / "openspec").mkdir()
    (repository / "openspec" / "config.yaml").write_text("schema: spec-driven\n")
    git(repository, "add", ".")
    commit(repository, "initialize target")
    original_commit = git(repository, "rev-parse", "HEAD")
    run_id = "webhook-run-1"

    source = WebhookSource(
        source_id="monitor",
        secret=SecretStr(WEBHOOK_SECRET),
        allowed_repositories=(repository,),
    )
    store = FactoryStore(tmp_path / "factory.sqlite3")
    endpoint = WebhookEndpoint(store, sources=(source,))
    alert = WebhookAlertPayload(
        repository=str(repository),
        description="Implement a small alert-handling feature",
    )
    body = alert.model_dump_json().encode("utf-8")
    timestamp = str(int(datetime.now(UTC).timestamp()))
    signature = hmac.new(
        WEBHOOK_SECRET.encode("utf-8"),
        timestamp.encode("ascii") + b".monitor.monitor-e2e-event." + body,
        hashlib.sha256,
    ).hexdigest()
    response = endpoint.handle(
        "monitor",
        {
            "x-factory-timestamp": timestamp,
            "x-factory-event-id": "monitor-e2e-event",
            "x-factory-signature": f"sha256={signature}",
        },
        body,
    )
    assert response.status_code == 202
    ingestion = WebhookIngestor(store, sources=(source,)).process_pending()
    assert len(ingestion) == 1 and ingestion[0].accepted
    request = store.list_queued_requests()[0]
    assert request.source is RequestSource.MONITORING

    triage = triage_request(
        request,
        outcome=TriageOutcome.SPECS_REQUIRED,
        rationale="The alert needs a detailed, human-approved implementation plan.",
    )
    planning_module = __import__(
        "cronos_ai.planning", fromlist=["subprocess"]
    )
    actual_subprocess_run = subprocess.run

    def fake_openspec_cli(command: list[str], **kwargs: Any):
        if command[0] == "openspec":
            if command[1] == "new":
                (Path(kwargs["cwd"]) / "openspec" / "changes" / command[3]).mkdir(
                    parents=True
                )
            return subprocess.CompletedProcess(command, 0, "", "")
        return actual_subprocess_run(command, **kwargs)

    monkeypatch.setattr(planning_module.subprocess, "run", fake_openspec_cli)
    run_branch = create_run_branch_with_plan(
        triage.request,
        triage,
        tmp_path / "run-worktrees" / run_id,
    )
    plan_context = run_branch.to_context(
        run_id,
        task_worktree_root=tmp_path / "task-worktrees",
        triage_result=triage,
    )
    graph = ExecutionGraph(store)
    graph.initialize_run(
        run_id,
        triage.request,
        run_branch.plan.plan,
        run_context=plan_context,
        triage_result=triage,
    )
    worker_ids = ("worker-1",)
    scheduler = BoundedTaskScheduler(
        run_id,
        store,
        graph,
        worker_ids=worker_ids,
        max_concurrency=1,
        max_attempts=3,
    )
    assert scheduler.dispatch_ready() == []

    store.enqueue_control_action(
        ControlAction(
            action_id="approve-plan-e2e",
            action_type=ControlActionType.APPROVE_PLAN,
            target_id=run_branch.plan.change_name,
            run_id=run_id,
            payload={
                "plan_hash": run_branch.plan.plan_hash,
                "reviewer": "e2e-reviewer",
            },
        )
    )
    assert HumanActionProcessor(store).process_pending()[0].status.value == "done"
    first_reservation = scheduler.dispatch_ready()
    assert len(first_reservation) == 1
    assert first_reservation[0].task_id == "2.1"

    store.close()
    store = FactoryStore(tmp_path / "factory.sqlite3")
    herdr_api = FakeHerdrSocket()
    herdr = HerdrAdapter(
        "factory-e2e",
        store,
        socket_client=herdr_api,
        startup_timeout=0.1,
    )
    recovery = TaskRecoveryManager(store, max_attempts=3).recover(run_id, herdr)
    assert recovery[0].disposition is RecoveryDisposition.RETRY
    assert store.get_attempts(run_id, "2.1")[-1].status.value == "interrupted"

    graph = ExecutionGraph(store)
    scheduler = BoundedTaskScheduler(
        run_id,
        store,
        graph,
        worker_ids=worker_ids,
        max_concurrency=1,
        max_attempts=3,
    )
    task_manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")
    sandbox = FakeSandbox()
    profile = SpecialistProfile(
        name="implementer",
        role="Implementation",
        model="fake-model",
        provider="fake-provider",
        provider_api_key_env="FACTORY_E2E_PROVIDER_KEY",
    )
    monkeypatch.setenv("FACTORY_E2E_PROVIDER_KEY", "test-only-worker-key")
    worker = PiWorkerExecutor(
        herdr,
        sandbox,  # type: ignore[arg-type]
        profile,
        pi_executable=sys.executable,
        command=(sys.executable, "-u", "-c", FAKE_PI_RPC),
    )
    integrator = TaskIntegrator(
        run_id,
        run_branch.branch_name,
        run_branch.worktree_path,
        store,
        starting_commit=run_branch.starting_commit,
        task_worktree_root=task_manager.worktree_root,
    )

    def run_workers() -> dict[str, Any]:
        task_worktrees: dict[str, Any] = {}

        def execute(assignment):
            task_worktree = task_manager.create(
                run_branch.branch_name,
                assignment.task_id,
                attempt_number=assignment.attempt_number,
            )
            task_worktrees[assignment.task_id] = task_worktree
            return worker.execute(
                task_worktree.path,
                worker_id=assignment.worker_id,
                task_id=assignment.task_id,
                prompt=f"task_id={assignment.task_id}; implement and verify",
                timeout=10,
            )

        results = scheduler.run_ready(execute)
        for task_result in results:
            assert task_result.success
            integrator.integrate(task_worktrees[task_result.assignment.task_id])
        return task_worktrees

    first_layer = run_workers()
    assert first_layer.keys() == {"2.1"}
    second_layer = run_workers()
    assert second_layer.keys() == {"2.2"}
    assert all(
        task.state is TaskState.REVIEW
        for task in store.list_tasks(run_id)
        if task.task_id in ("2.1", "2.2")
    )
    assert git(repository, "branch", "--show-current") == "main"
    assert git(repository, "rev-parse", "HEAD") == original_commit
    assert "task-2.1.txt" in git(
        run_branch.worktree_path, "ls-tree", "--name-only", "HEAD"
    )
    assert "task-2.2.txt" in git(
        run_branch.worktree_path, "ls-tree", "--name-only", "HEAD"
    )

    review = ReviewCoordinator(store)
    packet = review.prepare_review(
        run_id,
        review_summary="The integrated alert feature and verification are ready.",
        review_findings=("Implementation remains within the accepted request.",),
        verification_checks=(
            VerificationCheck(
                name="worker integration",
                command="fake Pi RPC and Git integration",
                passed=True,
                summary="Both dependency-ordered tasks completed and merged.",
            ),
        ),
        user_scenarios=(
            UserScenarioCheck(
                scenario="The user can use the generated feature.",
                passed=True,
                evidence="Both task result files are present on the run branch.",
            ),
        ),
    )
    ci = FakeCIAdapter(dispatch_status=DeliveryStatus.SUCCEEDED)
    delivery = DeliveryCoordinator(store, adapter=ci)
    assert not delivery.is_delivered(run_id)
    assert all(
        store.get_task(run_id, task_id).state is TaskState.REVIEW
        for task_id in ("2.1", "2.2")
    )
    store.enqueue_control_action(
        ControlAction(
            action_id="approve-review-e2e",
            action_type=ControlActionType.APPROVE_REVIEW,
            target_id="run",
            run_id=run_id,
            payload={"review_hash": packet.review_hash, "reviewer": "e2e-reviewer"},
        )
    )
    assert HumanActionProcessor(store).process_pending()[0].status.value == "done"
    assert all(
        store.get_task(run_id, task_id).state is TaskState.DONE
        for task_id in ("2.1", "2.2")
    )
    assert not delivery.is_delivered(run_id)

    delivery_record = delivery.dispatch(run_id)

    assert delivery_record.status is DeliveryStatus.SUCCEEDED
    assert delivery.is_delivered(run_id)
    assert ci.dispatch_calls == 1
    assert store.list_received_webhook_events() == []
    store.close()
