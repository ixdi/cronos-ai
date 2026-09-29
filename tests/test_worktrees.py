import subprocess
from pathlib import Path

import pytest

from cronos_ai.worktrees import TaskWorktreeError, TaskWorktreeManager


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def create_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(path, "config", "user.name", "factory-test")
    git(path, "config", "user.email", "factory-test@example.invalid")
    (path / "README.md").write_text("Starting state\n")
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
    git(path, "branch", "factory/run/run-1")
    return path.resolve()


def test_concurrent_tasks_get_distinct_worktrees_from_the_run_branch(
    tmp_path: Path,
) -> None:
    repository = create_repository(tmp_path / "project")
    original_branch = git(repository, "branch", "--show-current")
    original_commit = git(repository, "rev-parse", "HEAD")
    run_branch_worktree = tmp_path / "run-branch-worktree"
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "worktree",
            "add",
            "--quiet",
            str(run_branch_worktree),
            "factory/run/run-1",
        ],
        check=True,
    )
    (run_branch_worktree / "run-plan.txt").write_text("Committed run plan\n")
    subprocess.run(
        ["git", "-C", str(run_branch_worktree), "add", "run-plan.txt"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(run_branch_worktree),
            "-c",
            "user.name=factory-test",
            "-c",
            "user.email=factory-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "commit run plan",
        ],
        check=True,
    )
    run_branch_commit = git(run_branch_worktree, "rev-parse", "HEAD")
    manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")

    first = manager.create("factory/run/run-1", "1.1", attempt_number=1)
    second = manager.create("factory/run/run-1", "1.2", attempt_number=1)

    assert first.path != second.path
    assert first.branch_name != second.branch_name
    assert first.starting_commit == run_branch_commit
    assert second.starting_commit == run_branch_commit
    assert (first.path / "run-plan.txt").is_file()
    assert (second.path / "run-plan.txt").is_file()
    assert git(first.path, "branch", "--show-current") == first.branch_name
    assert git(second.path, "branch", "--show-current") == second.branch_name
    assert git(repository, "branch", "--show-current") == original_branch
    assert git(repository, "rev-parse", "HEAD") == original_commit
    assert git(repository, "status", "--porcelain=v1") == ""


def test_cleanup_removes_only_the_task_worktree_and_keeps_its_branch(
    tmp_path: Path,
) -> None:
    repository = create_repository(tmp_path / "project")
    manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")
    task_worktree = manager.create("factory/run/run-1", "1.1", attempt_number=1)

    manager.cleanup(task_worktree)

    assert not task_worktree.path.exists()
    assert git(repository, "show-ref", "--verify", task_worktree.branch_ref)
    assert git(repository, "status", "--porcelain=v1") == ""


def test_cleanup_refuses_to_discard_uncommitted_task_changes(tmp_path: Path) -> None:
    repository = create_repository(tmp_path / "project")
    manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")
    task_worktree = manager.create("factory/run/run-1", "1.1", attempt_number=1)
    (task_worktree.path / "README.md").write_text("Uncommitted work\n")

    with pytest.raises(TaskWorktreeError, match="uncommitted changes"):
        manager.cleanup(task_worktree)

    assert (task_worktree.path / "README.md").read_text() == "Uncommitted work\n"
    assert task_worktree.path.is_dir()


def test_worktree_root_must_be_outside_the_original_checkout(tmp_path: Path) -> None:
    repository = create_repository(tmp_path / "project")

    with pytest.raises(TaskWorktreeError, match="outside the original checkout"):
        TaskWorktreeManager(repository, repository / ".factory-tasks")
