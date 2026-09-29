import subprocess
from pathlib import Path

import pytest

from cronos_ai.intake import IntakeError, intake_request
from cronos_ai.models import RequestSource, WorkRequest


def create_initialized_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(path)],
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
    openspec = path / "openspec"
    openspec.mkdir()
    (openspec / "config.yaml").write_text("schema: spec-driven\n")
    subprocess.run(
        ["git", "-C", str(path), "add", "README.md", "openspec/config.yaml"],
        check=True,
    )
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
    return path.resolve()


def test_intake_returns_request_for_clean_initialized_repository(
    tmp_path: Path,
) -> None:
    repository = create_initialized_repository(tmp_path / "project")

    request = intake_request(repository, "Add a small feature")

    assert isinstance(request, WorkRequest)
    assert request.repo_path == repository
    assert request.description == "Add a small feature"
    assert request.source is RequestSource.USER


def test_intake_requires_an_explicit_repository_path() -> None:
    with pytest.raises(IntakeError, match="explicit repository path"):
        intake_request(None, "Add a feature")


def test_intake_rejects_uninitialized_openspec_without_changes(tmp_path: Path) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    before = sorted(path.relative_to(repository) for path in repository.rglob("*"))

    with pytest.raises(IntakeError, match="initialized OpenSpec"):
        intake_request(repository, "Add a feature")

    after = sorted(path.relative_to(repository) for path in repository.rglob("*"))
    assert after == before


def test_intake_rejects_dirty_repository_without_changes(tmp_path: Path) -> None:
    repository = create_initialized_repository(tmp_path / "project")
    (repository / "README.md").write_text("Uncommitted edit\n")
    before = (repository / "README.md").read_text()

    with pytest.raises(IntakeError, match="clean working tree"):
        intake_request(repository, "Add a feature")

    assert (repository / "README.md").read_text() == before
    assert not (repository / "openspec" / "changes").exists()


def test_intake_rejects_untracked_files(tmp_path: Path) -> None:
    repository = create_initialized_repository(tmp_path / "project")
    untracked = repository / "notes.txt"
    untracked.write_text("Keep this file\n")

    with pytest.raises(IntakeError, match="clean working tree"):
        intake_request(repository, "Add a feature")

    assert untracked.read_text() == "Keep this file\n"
