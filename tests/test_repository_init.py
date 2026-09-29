import subprocess
from pathlib import Path

import pytest

from cronos_ai.repository_init import (
    RepositoryInitializationError,
    initialize_repository,
)


def create_git_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return path.resolve()


def test_init_runs_openspec_only_for_the_explicit_repository(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    selected_repo = create_git_repository(tmp_path / "selected")
    other_repo = create_git_repository(tmp_path / "other")
    calls: list[list[str]] = []
    real_run = subprocess.run

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] == "git":
            return real_run(command, **kwargs)
        calls.append(command)
        openspec_root = Path(command[2]) / "openspec"
        openspec_root.mkdir()
        (openspec_root / "config.yaml").write_text("schema: spec-driven\n")
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr("cronos_ai.repository_init.subprocess.run", run)

    result = initialize_repository(selected_repo)

    assert result == selected_repo
    assert calls == [["openspec", "init", str(selected_repo), "--tools", "none"]]
    assert (selected_repo / "openspec" / "config.yaml").is_file()
    assert not (other_repo / "openspec").exists()


def test_init_rejects_a_non_git_directory_without_modifying_it(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    target = tmp_path / "not-a-repository"
    target.mkdir()
    calls: list[list[str]] = []
    real_run = subprocess.run

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] != "git":
            calls.append(command)
        return real_run(command, **kwargs)

    monkeypatch.setattr("cronos_ai.repository_init.subprocess.run", run)

    with pytest.raises(RepositoryInitializationError, match="Git working tree"):
        initialize_repository(target)

    assert list(target.iterdir()) == []
    assert calls == []


def test_init_requires_repository_root_not_nested_directory(tmp_path: Path) -> None:
    repository = create_git_repository(tmp_path / "repository")
    nested = repository / "nested"
    nested.mkdir()

    with pytest.raises(RepositoryInitializationError, match="repository root"):
        initialize_repository(nested)


def test_init_is_idempotent_for_an_existing_openspec_root(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = create_git_repository(tmp_path / "repository")
    openspec_root = repository / "openspec"
    openspec_root.mkdir()
    (openspec_root / "config.yaml").write_text("schema: spec-driven\n")

    real_run = subprocess.run

    def fail_if_called(
        command: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        if command[0] == "git":
            return real_run(command, **kwargs)
        raise AssertionError("OpenSpec should not run for an initialized repository")

    monkeypatch.setattr(
        "cronos_ai.repository_init.subprocess.run",
        fail_if_called,
    )

    assert initialize_repository(repository) == repository
