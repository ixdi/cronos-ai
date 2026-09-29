import shutil
import subprocess
from pathlib import Path

import pytest

from cronos_ai.models import TriageOutcome, WorkRequest
from cronos_ai.run_branch import RunBranchError, create_run_branch_with_plan
from cronos_ai.triage import triage_request


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def create_initialized_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(path, "config", "user.name", "factory-test")
    git(path, "config", "user.email", "factory-test@example.invalid")
    (path / "README.md").write_text("Initial project\n")
    git(path, "add", "README.md")
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
    git(path, "add", "openspec")
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


@pytest.mark.skipif(shutil.which("openspec") is None, reason="OpenSpec CLI unavailable")
def test_run_branch_commits_plan_without_changing_original_checkout(
    tmp_path: Path,
) -> None:
    repository = create_initialized_repository(tmp_path / "project")
    original_branch = git(repository, "branch", "--show-current")
    original_commit = git(repository, "rev-parse", "HEAD")
    original_status = git(
        repository,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    )
    request = WorkRequest(
        request_id="request-12345678",
        description="Improve the settings page",
        repo_path=repository,
    )
    triage = triage_request(
        request,
        TriageOutcome.ACTIONABLE,
        rationale="The requested change is bounded.",
    )
    worktree_path = tmp_path / "run-worktrees" / "request-12345678"

    run = create_run_branch_with_plan(request, triage, worktree_path)

    assert git(repository, "branch", "--show-current") == original_branch
    assert git(repository, "rev-parse", "HEAD") == original_commit
    assert (
        git(
            repository,
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        )
        == original_status
        == ""
    )
    assert git(run.worktree_path, "branch", "--show-current") == run.branch_name
    assert run.starting_commit == original_commit
    assert run.plan.change_dir.is_dir()
    assert run.plan.change_dir.is_relative_to(run.worktree_path)
    assert git(run.worktree_path, "status", "--porcelain=v1") == ""
    assert git(run.worktree_path, "log", "-1", "--format=%s").startswith(
        "chore(openspec): plan "
    )
    assert (
        git(
            run.worktree_path,
            "show",
            "-s",
            "--format=%an <%ae>",
        )
        == "Cronos AI <cronos-ai@localhost>"
    )
    assert run.plan.plan_hash
    context = run.to_context(
        "run-1",
        task_worktree_root=tmp_path / "task-worktrees",
        approval_required=True,
    )
    assert context.branch_name == run.branch_name
    assert context.run_worktree_path == run.worktree_path.resolve()
    assert context.starting_commit == original_commit
    assert context.task_worktree_root == (tmp_path / "task-worktrees").resolve()
    assert context.plan_hash == run.plan.plan_hash
    assert context.approval_required


@pytest.mark.skipif(shutil.which("openspec") is None, reason="OpenSpec CLI unavailable")
def test_dirty_checkout_is_rejected_before_creating_a_run_branch(
    tmp_path: Path,
) -> None:
    repository = create_initialized_repository(tmp_path / "project")
    original_branch = git(repository, "branch", "--show-current")
    original_commit = git(repository, "rev-parse", "HEAD")
    (repository / "README.md").write_text("Uncommitted edit\n")
    original_status = git(repository, "status", "--porcelain=v1")
    request = WorkRequest(
        request_id="request-dirty",
        description="Improve the settings page",
        repo_path=repository,
    )
    triage = triage_request(
        request,
        TriageOutcome.ACTIONABLE,
        rationale="The requested change is bounded.",
    )

    with pytest.raises(RunBranchError, match="clean working tree"):
        create_run_branch_with_plan(
            request,
            triage,
            tmp_path / "run-worktrees" / "dirty",
        )

    assert git(repository, "branch", "--show-current") == original_branch
    assert git(repository, "rev-parse", "HEAD") == original_commit
    assert git(repository, "status", "--porcelain=v1") == original_status
    assert not (tmp_path / "run-worktrees").exists()


def test_worktree_must_be_outside_the_original_checkout(tmp_path: Path) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    request = WorkRequest(
        request_id="request-1",
        description="Add a feature",
        repo_path=repository,
    )
    triage = triage_request(
        request,
        TriageOutcome.ACTIONABLE,
        rationale="The request is bounded.",
    )

    with pytest.raises(RunBranchError, match="outside the original checkout"):
        create_run_branch_with_plan(
            request,
            triage,
            repository / ".factory-run",
        )

    assert list(repository.iterdir()) == []
