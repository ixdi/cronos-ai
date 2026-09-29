import json
import os
import subprocess
from pathlib import Path
from typing import Any

import pytest

from cronos_ai.herdr import HerdrAdapter
from cronos_ai.models import (
    OpenSpecPlan,
    PlanTask,
    TaskRecord,
    TaskState,
    WorkRequest,
)
from cronos_ai.profiles import FactoryProfileRegistry
from cronos_ai.sandbox import DockerSandboxAdapter
from cronos_ai.storage import FactoryStore
from cronos_ai.worker_execution import PiWorkerExecutor
from cronos_ai.worktrees import TaskWorktreeManager

FAKE_PI_RPC = """import json
import os
import pathlib
import sys

for line in sys.stdin:
    command = json.loads(line)
    if command.get('type') != 'prompt':
        continue
    try:
        pathlib.Path('/root/.ssh').stat()
        host_auth_accessible = True
    except OSError:
        host_auth_accessible = False
    skill_index = sys.argv.index('--skill')
    skill_path = pathlib.Path(sys.argv[skill_index + 1])
    assert skill_path.joinpath('SKILL.md').read_text() == (
        'approved factory skill' + chr(10)
    )
    assert '--no-skills' in sys.argv
    assert '--no-extensions' in sys.argv
    assert '--no-context-files' in sys.argv
    pathlib.Path('/workspace/factory-result.txt').write_text(
        'rpc task completed\\n' + f'host_auth_accessible={host_auth_accessible}\\n'
    )
    print(json.dumps({
        'id': command['id'],
        'type': 'response',
        'command': 'prompt',
        'success': True,
    }), flush=True)
    print(json.dumps({
        'type': 'tool_execution_start',
        'toolCallId': 'write-1',
        'toolName': 'write',
        'args': {'path': 'factory-result.txt'},
    }), flush=True)
    print(json.dumps({
        'type': 'tool_execution_end',
        'toolCallId': 'write-1',
        'toolName': 'write',
        'result': {'content': [{'type': 'text', 'text': 'written'}]},
        'isError': False,
    }), flush=True)
    print(json.dumps({
        'type': 'message_end',
        'message': {'role': 'assistant', 'content': [{'type': 'text', 'text': 'Done'}]},
    }), flush=True)
    print(json.dumps({'type': 'agent_settled'}), flush=True)
"""


class FakeHerdrSocket:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.pane = {
            "workspace_id": "w1",
            "pane_id": "w1:p1",
            "agent": None,
            "agent_status": "idle",
        }

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((method, params))
        if method == "ping":
            return {"type": "pong"}
        if method == "workspace.create":
            return {
                "workspace": {"workspace_id": "w1"},
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


def create_run_branch(repository: Path) -> str:
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    subprocess.run(
        ["git", "-C", str(repository), "config", "user.name", "factory-test"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "config",
            "user.email",
            "factory-test@example.invalid",
        ],
        check=True,
    )
    (repository / "README.md").write_text("factory task\n")
    (repository / "factory-result.txt").write_text("")
    unlisted_skill = repository / ".agents" / "skills" / "unlisted"
    unlisted_skill.mkdir(parents=True)
    (unlisted_skill / "SKILL.md").write_text("unlisted project skill\n")
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "add",
            "README.md",
            "factory-result.txt",
            ".agents",
        ],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
        check=True,
    )
    run_branch = "factory/run/e2e"
    subprocess.run(["git", "-C", str(repository), "branch", run_branch], check=True)
    return run_branch


@pytest.mark.skipif(
    os.environ.get("CRONOS_AI_DOCKER_E2E") != "1",
    reason="set CRONOS_AI_DOCKER_E2E=1 with a running Docker daemon",
)
def test_worker_task_runs_through_herdr_rpc_and_sandbox_end_to_end(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = tmp_path / "project"
    run_branch = create_run_branch(repository)
    task_manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")
    task_worktree = task_manager.create(run_branch, "task-1", attempt_number=1)
    herdr_api = FakeHerdrSocket()
    store = FactoryStore(tmp_path / "factory.sqlite3")
    store.create_run(
        "run-1",
        WorkRequest(
            request_id="request-1",
            description="Run the sandbox worker",
            repo_path=repository,
        ),
        OpenSpecPlan(
            change_name="worker-execution",
            tasks=(PlanTask(task_id="task-1", description="Run worker"),),
        ),
        (TaskRecord(task_id="task-1", state=TaskState.READY),),
    )
    herdr = HerdrAdapter(
        "software-factory",
        store,
        socket_client=herdr_api,
        startup_timeout=0.1,
    )
    factory_root = tmp_path / "factory-resources"
    factory_skill = factory_root / "skills" / "approved"
    factory_skill.mkdir(parents=True)
    (factory_skill / "SKILL.md").write_text("approved factory skill\n")
    (factory_root / "profiles.json").write_text(
        json.dumps(
            {
                "profiles": {
                    "implementer": {
                        "name": "implementer",
                        "role": "Implementation",
                        "model": "offline-test-model",
                        "provider": "offline-test-provider",
                        "provider_api_key_env": "FACTORY_E2E_PROVIDER_KEY",
                        "skills": ["approved"],
                        "mcp_tools": [],
                        "network_allowlist": [],
                    }
                },
                "mcp_servers": [],
            }
        )
    )
    resources = FactoryProfileRegistry(factory_root).resolve(
        "implementer",
        target_repository=task_worktree.path,
    )
    profile = resources.profile
    monkeypatch.setenv("FACTORY_E2E_PROVIDER_KEY", "sandbox-e2e-key")
    sandbox = DockerSandboxAdapter("python:3.12-alpine3.22")
    executor = PiWorkerExecutor(
        herdr,
        sandbox,
        profile,
        resources=resources,
        command=(
            "python3",
            "-u",
            "-c",
            FAKE_PI_RPC,
            "--no-skills",
            "--no-extensions",
            "--no-context-files",
            "--skill",
            str(resources.skill_paths[0]),
        ),
    )

    result = executor.execute(
        task_worktree.path,
        run_id="run-1",
        worker_id="worker-1",
        task_id="task-1",
        prompt="Implement one small isolated change",
        timeout=30,
    )
    diff = subprocess.run(
        ["git", "-C", str(task_worktree.path), "diff", "--", "factory-result.txt"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout

    assert result.rpc_result.settled is True
    assert "rpc task completed" in diff
    assert "host_auth_accessible=False" in diff
    assert any(
        method == "pane.report_agent" and params.get("state") == "working"
        for method, params in herdr_api.calls
    )
    assert any(
        method == "pane.report_metadata"
        for method, _params in herdr_api.calls
    )
    store.close()
