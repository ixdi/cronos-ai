import subprocess
import sys
from pathlib import Path

import pytest

from cronos_ai.cli import main
from cronos_ai.controller import LocalController, default_state_dir
from cronos_ai.models import (
    ControlAction,
    ControlActionType,
    TriageOutcome,
    WorkRequest,
)
from cronos_ai.storage import ControllerLockError, FactoryStore


def create_clean_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "factory-test"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(path),
            "config",
            "user.email",
            "factory-test@example.invalid",
        ],
        check=True,
    )
    (path / "README.md").write_text("Project\n")
    subprocess.run(["git", "-C", str(path), "add", "README.md"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(path),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
        check=True,
    )
    subprocess.run(
        ["openspec", "init", str(path), "--tools", "none"],
        check=True,
        capture_output=True,
        text=True,
    )
    subprocess.run(["git", "-C", str(path), "add", "openspec"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(path),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "initialize OpenSpec",
        ],
        check=True,
    )
    return path.resolve()


def test_run_command_persists_request_and_status_reports_queue(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    repository = create_clean_repository(tmp_path / "project")
    state_dir = tmp_path / "factory-state"

    main(
        [
            "run",
            "--repo",
            str(repository),
            "--request",
            "Improve the settings page",
            "--state-dir",
            str(state_dir),
        ]
    )

    assert "Queued request" in capsys.readouterr().out
    with FactoryStore(state_dir / "factory.sqlite3") as store:
        requests = store.list_requests()
    assert len(requests) == 1
    assert requests[0].repo_path == repository
    assert requests[0].description == "Improve the settings page"

    main(["status", "--state-dir", str(state_dir)])
    status = capsys.readouterr().out
    assert "Queued requests: 1" in status
    assert "Controller: not running" in status


def test_human_action_is_recorded_durably(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    state_dir = tmp_path / "factory-state"

    main(
        [
            "action",
            "--action",
            "clarify-request",
            "--target",
            "request-1",
            "--data",
            "answer=Use the current account default",
            "--state-dir",
            str(state_dir),
        ]
    )

    assert "Queued human action" in capsys.readouterr().out
    with FactoryStore(state_dir / "factory.sqlite3") as store:
        actions = store.list_pending_actions()
    assert len(actions) == 1
    assert actions[0].action_type is ControlActionType.CLARIFY_REQUEST
    assert actions[0].target_id == "request-1"
    assert actions[0].payload == {"answer": "Use the current account default"}
    assert actions[0].actor == "local-operator"

    main(["actions", "--state-dir", str(state_dir)])
    assert "clarify-request" in capsys.readouterr().out


def test_attention_cli_outputs_structured_empty_queue(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    main(["attention", "--json", "--state-dir", str(tmp_path)])

    assert capsys.readouterr().out.strip() == "[]"


def test_second_controller_cannot_acquire_active_controller_lock(
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "factory-state"

    with LocalController(state_dir):
        with pytest.raises(ControllerLockError):
            with LocalController(state_dir):
                pytest.fail("second controller unexpectedly acquired the lock")

    with LocalController(state_dir):
        pass


def test_controller_processes_queued_human_actions(tmp_path: Path) -> None:
    state_dir = tmp_path / "factory-state"
    request = WorkRequest(
        request_id="clarification-request",
        description="Needs clarification",
        repo_path=tmp_path,
        triage_outcome=TriageOutcome.CLARIFICATION_REQUIRED,
    )
    with FactoryStore(state_dir / "factory.sqlite3") as store:
        store.enqueue_request(request)
        action = ControlAction(
            action_id="clarification-action",
            action_type=ControlActionType.CLARIFY_REQUEST,
            target_id=request.request_id,
            payload={"answer": "Use the existing account setting"},
        )
        store.enqueue_control_action(action)

    class StopAfterOneCycle:
        stopped = False

        def is_set(self) -> bool:
            return self.stopped

        def wait(self, timeout: float) -> bool:
            self.stopped = True
            return True

    LocalController(state_dir, heartbeat_interval=0.01).run_forever(
        StopAfterOneCycle()  # type: ignore[arg-type]
    )

    with FactoryStore(state_dir / "factory.sqlite3") as store:
        updated = store.list_requests()[0]
        audit = store.get_control_action_record("clarification-action")
    assert updated.triage_outcome is None
    assert "Use the existing account setting" in updated.description
    assert audit is not None and audit.status.value == "done"


def test_second_controller_process_is_rejected(tmp_path: Path) -> None:
    state_dir = tmp_path / "factory-state"

    with LocalController(state_dir):
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "cronos_ai",
                "controller",
                "start",
                "--state-dir",
                str(state_dir),
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )

    assert result.returncode == 1
    assert "another factory controller" in result.stderr


def test_default_state_directory_is_overridable_by_environment(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "custom-state"
    monkeypatch.setenv("CRONOS_AI_STATE_DIR", str(state_dir))

    assert default_state_dir() == state_dir
