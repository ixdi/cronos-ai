import shutil
import subprocess
import sys
from pathlib import Path

import pytest

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
