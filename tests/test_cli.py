import io
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

import cronos_ai.cli as cli_module
from cronos_ai.cli import main


def test_module_cli_displays_help() -> None:
    result = subprocess.run(
        [sys.executable, "-m", "cronos_ai", "--help"],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0
    assert "usage:" in result.stdout.lower()
    assert "usage: cronos-ai" in result.stdout


def test_module_cli_displays_version() -> None:
    result = subprocess.run(
        [sys.executable, "-m", "cronos_ai", "--version"],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0
    assert result.stdout == "cronos-ai 0.1.0\n"


def test_installed_cli_entry_point_displays_version() -> None:
    executable = shutil.which("cronos-ai")
    assert executable is not None

    result = subprocess.run(
        [executable, "--version"],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0
    assert "0.1.0" in result.stdout


def test_init_cli_requires_an_explicit_repository() -> None:
    with pytest.raises(SystemExit) as error:
        main(["init"])

    assert error.value.code == 2


def test_init_cli_reports_the_explicit_repository(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(
        "cronos_ai.cli.initialize_repository",
        lambda repo: repo,
    )

    main(["init", "--repo", str(tmp_path)])

    assert str(tmp_path) in capsys.readouterr().out


def test_dashboard_help_documents_interactive_command_and_state_override(
    capsys: pytest.CaptureFixture[str],
) -> None:
    with pytest.raises(SystemExit) as error:
        main(["dashboard", "--help"])

    assert error.value.code == 0
    output = capsys.readouterr().out
    assert "interactive terminal dashboard" in output.lower()
    assert "--state-dir" in output


def test_dashboard_rejects_noninteractive_terminal_without_opening_state(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(sys, "stdin", io.StringIO())
    state_dir = tmp_path / "factory-state"

    with pytest.raises(SystemExit) as error:
        main(["dashboard", "--state-dir", str(state_dir)])

    assert error.value.code == 1
    assert "interactive terminal" in capsys.readouterr().err.lower()
    assert not (state_dir / "factory.sqlite3").exists()


def test_dashboard_uses_explicit_state_override_and_runs_tui(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    class FakeTerminal(io.StringIO):
        def isatty(self) -> bool:
            return True

    created_databases: list[Path] = []
    closed_stores: list[bool] = []
    applications: list[object] = []

    class FakeStore:
        def __init__(self, database_path: Path) -> None:
            created_databases.append(database_path)

        def close(self) -> None:
            closed_stores.append(True)

    class FakeReadModel:
        def __init__(self, store: FakeStore) -> None:
            self.store = store

    class FakeApp:
        def __init__(self, read_model: FakeReadModel) -> None:
            self.read_model = read_model
            self.ran = False
            applications.append(self)

        def run(self) -> None:
            self.ran = True

    monkeypatch.setattr(sys, "stdin", FakeTerminal())
    monkeypatch.setattr(sys, "stdout", FakeTerminal())
    monkeypatch.setattr(cli_module, "FactoryStore", FakeStore, raising=False)
    monkeypatch.setattr(cli_module, "DashboardReadModel", FakeReadModel, raising=False)
    monkeypatch.setattr(cli_module, "FactoryDashboardApp", FakeApp, raising=False)
    state_dir = tmp_path / "custom-state"

    main(["dashboard", "--state-dir", str(state_dir)])

    assert created_databases == [state_dir / "factory.sqlite3"]
    assert closed_stores == [True]
    assert len(applications) == 1
    assert applications[0].ran


def test_existing_noninteractive_status_output_is_preserved(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    main(["status", "--state-dir", str(tmp_path / "state")])

    output = capsys.readouterr().out
    assert "Controller: not running" in output
    assert "Queued requests: 0" in output
    assert "Pending human actions: 0" in output
