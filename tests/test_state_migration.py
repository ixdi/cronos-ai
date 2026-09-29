import os
import subprocess
import sys
from pathlib import Path

import pytest

from cronos_ai.cli import main
from cronos_ai.controller import StateMigrationError, default_state_dir
from cronos_ai.models import WorkRequest
from cronos_ai.storage import FactoryStore


def test_default_state_directory_uses_xdg_state_home(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "xdg-state"))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)
    monkeypatch.delenv("AI_SOFTWARE_FACTORY_STATE_DIR", raising=False)

    assert default_state_dir() == tmp_path / "xdg-state" / "cronos-ai"


def test_default_state_directory_falls_back_to_local_state(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.delenv("XDG_STATE_HOME", raising=False)
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)
    monkeypatch.delenv("AI_SOFTWARE_FACTORY_STATE_DIR", raising=False)

    assert default_state_dir() == tmp_path / "home" / ".local" / "state" / "cronos-ai"


def test_cronos_ai_environment_override_is_used(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    configured = tmp_path / "custom-state"
    monkeypatch.setenv("CRONOS_AI_STATE_DIR", str(configured))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "xdg-state"))

    assert default_state_dir() == configured


def test_legacy_environment_override_is_ignored(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    legacy_override = tmp_path / "legacy-custom-state"
    monkeypatch.setenv("AI_SOFTWARE_FACTORY_STATE_DIR", str(legacy_override))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "xdg-state"))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    assert default_state_dir() == tmp_path / "xdg-state" / "cronos-ai"


def test_explicit_cli_state_dir_bypasses_default_migration(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    legacy_state_dir.mkdir(parents=True)
    (legacy_state_dir / "sentinel").write_text("preserve", encoding="utf-8")
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)
    custom_state_dir = tmp_path / "explicit-state"

    main(["status", "--state-dir", str(custom_state_dir)])

    assert (legacy_state_dir / "sentinel").read_text(encoding="utf-8") == "preserve"
    assert not (xdg_state_home / "cronos-ai").exists()
    assert (custom_state_dir / "factory.sqlite3").is_file()


def test_environment_override_bypasses_default_migration(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    legacy_state_dir.mkdir(parents=True)
    (legacy_state_dir / "sentinel").write_text("preserve", encoding="utf-8")
    configured = tmp_path / "environment-state"
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.setenv("CRONOS_AI_STATE_DIR", str(configured))

    assert default_state_dir() == configured
    assert (legacy_state_dir / "sentinel").read_text(encoding="utf-8") == "preserve"
    assert not (xdg_state_home / "cronos-ai").exists()


def test_migrates_legacy_state_and_preserves_queue_and_schema(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    legacy_state_dir.mkdir(parents=True)
    (legacy_state_dir / "sentinel").write_text("preserve", encoding="utf-8")
    database = legacy_state_dir / "factory.sqlite3"
    with FactoryStore(database) as store:
        schema_version = store.schema_version
        store.enqueue_request(
            WorkRequest(
                request_id="preserved-request",
                description="Preserve this queued request",
                repo_path=tmp_path,
            )
        )
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    migrated_state_dir = default_state_dir()

    assert migrated_state_dir == xdg_state_home / "cronos-ai"
    assert not legacy_state_dir.exists()
    assert (migrated_state_dir / "sentinel").read_text(encoding="utf-8") == "preserve"
    with FactoryStore(migrated_state_dir / "factory.sqlite3") as store:
        assert store.schema_version == schema_version
        assert [request.request_id for request in store.list_queued_requests()] == [
            "preserved-request"
        ]


def test_existing_cronos_ai_state_is_reused_without_migration(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    state_dir = xdg_state_home / "cronos-ai"
    state_dir.mkdir(parents=True)
    (state_dir / "sentinel").write_text("new-state", encoding="utf-8")
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    assert default_state_dir() == state_dir
    assert (state_dir / "sentinel").read_text(encoding="utf-8") == "new-state"
    assert not (xdg_state_home / "ai-software-factory").exists()


def test_no_prior_state_uses_new_directory_and_creates_database_on_command(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    assert default_state_dir() == xdg_state_home / "cronos-ai"
    main(["status"])

    assert (xdg_state_home / "cronos-ai" / "factory.sqlite3").is_file()
    assert not (xdg_state_home / "ai-software-factory").exists()


def test_both_default_state_directories_fail_without_modifying_either(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    new_state_dir = xdg_state_home / "cronos-ai"
    legacy_state_dir.mkdir(parents=True)
    new_state_dir.mkdir()
    (legacy_state_dir / "sentinel").write_text("old", encoding="utf-8")
    (new_state_dir / "sentinel").write_text("new", encoding="utf-8")
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    with pytest.raises(
        StateMigrationError, match="both default state directories exist"
    ):
        default_state_dir()

    assert (legacy_state_dir / "sentinel").read_text(encoding="utf-8") == "old"
    assert (new_state_dir / "sentinel").read_text(encoding="utf-8") == "new"


def test_preexisting_empty_destination_is_not_replaced(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    new_state_dir = xdg_state_home / "cronos-ai"
    legacy_state_dir.mkdir(parents=True)
    new_state_dir.mkdir()
    (legacy_state_dir / "sentinel").write_text("old", encoding="utf-8")
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    with pytest.raises(StateMigrationError):
        default_state_dir()

    assert (legacy_state_dir / "sentinel").read_text(encoding="utf-8") == "old"
    assert new_state_dir.is_dir()
    assert list(new_state_dir.iterdir()) == []


def test_cli_reports_actionable_error_when_default_directories_conflict(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    (xdg_state_home / "ai-software-factory").mkdir(parents=True)
    (xdg_state_home / "cronos-ai").mkdir()
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    with pytest.raises(SystemExit) as error:
        main(["status"])

    assert error.value.code == 1
    assert "Back up both directories" in capsys.readouterr().err


def test_legacy_symlink_is_refused_without_touching_its_target(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    xdg_state_home.mkdir()
    external_state = tmp_path / "external-state"
    external_state.mkdir()
    (external_state / "sentinel").write_text("external", encoding="utf-8")
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    legacy_state_dir.symlink_to(external_state, target_is_directory=True)
    monkeypatch.setenv("XDG_STATE_HOME", str(xdg_state_home))
    monkeypatch.delenv("CRONOS_AI_STATE_DIR", raising=False)

    with pytest.raises(StateMigrationError, match="symbolic link"):
        default_state_dir()

    assert legacy_state_dir.is_symlink()
    assert (external_state / "sentinel").read_text(encoding="utf-8") == "external"
    assert not (xdg_state_home / "cronos-ai").exists()


def test_concurrent_default_resolution_migrates_state_once(
    tmp_path: Path,
) -> None:
    xdg_state_home = tmp_path / "xdg-state"
    legacy_state_dir = xdg_state_home / "ai-software-factory"
    legacy_state_dir.mkdir(parents=True)
    (legacy_state_dir / "sentinel").write_text("once", encoding="utf-8")
    environment = os.environ.copy()
    environment["XDG_STATE_HOME"] = str(xdg_state_home)
    environment.pop("CRONOS_AI_STATE_DIR", None)
    environment.pop("AI_SOFTWARE_FACTORY_STATE_DIR", None)
    code = (
        "from cronos_ai.controller import default_state_dir; print(default_state_dir())"
    )
    processes = [
        subprocess.Popen(
            [sys.executable, "-c", code],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(2)
    ]

    results = [process.communicate(timeout=10) for process in processes]

    assert all(process.returncode == 0 for process in processes), results
    assert [stdout.strip() for stdout, _ in results] == [
        str(xdg_state_home / "cronos-ai"),
        str(xdg_state_home / "cronos-ai"),
    ]
    assert (xdg_state_home / "cronos-ai" / "sentinel").read_text(
        encoding="utf-8"
    ) == "once"
    assert not legacy_state_dir.exists()
