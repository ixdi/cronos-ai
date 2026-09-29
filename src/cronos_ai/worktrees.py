"""Isolated implementation-task Git worktrees."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path


class TaskWorktreeError(RuntimeError):
    """Raised when a task worktree cannot be created or safely removed."""


@dataclass(frozen=True)
class TaskWorktree:
    """A task branch checked out in its own writable Git worktree."""

    repository: Path
    worktree_root: Path
    run_branch: str
    task_id: str
    attempt_number: int
    branch_name: str
    path: Path
    starting_commit: str

    @property
    def branch_ref(self) -> str:
        """Return the fully qualified Git branch reference."""
        return f"refs/heads/{self.branch_name}"


class TaskWorktreeManager:
    """Create task worktrees without writing into the original checkout."""

    def __init__(self, repository: Path, worktree_root: Path) -> None:
        try:
            self.repository = repository.resolve(strict=True)
        except OSError as error:
            raise TaskWorktreeError("Git repository does not exist") from error
        if not self.repository.is_dir():
            raise TaskWorktreeError("Git repository path is not a directory")

        result = self._git(self.repository, "rev-parse", "--show-toplevel")
        if Path(result.stdout.strip()).resolve() != self.repository:
            raise TaskWorktreeError("repository path must be the Git root")

        self.worktree_root = worktree_root.resolve()
        if self.worktree_root.is_relative_to(self.repository):
            raise TaskWorktreeError(
                "task worktrees must be outside the original checkout"
            )

    @staticmethod
    def _git(repository: Path, *args: str) -> subprocess.CompletedProcess[str]:
        try:
            result = subprocess.run(
                ["git", *args],
                cwd=repository,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as error:
            raise TaskWorktreeError("Git is unavailable") from error
        if result.returncode != 0:
            details = result.stderr.strip() or result.stdout.strip()
            message = f"Git command failed: {' '.join(args)}"
            if details:
                message = f"{message}: {details}"
            raise TaskWorktreeError(message)
        return result

    @staticmethod
    def _slug(value: str) -> str:
        return re.sub(r"[^a-z0-9]+", "-", value.casefold()).strip("-") or "task"

    @classmethod
    def branch_name(cls, run_branch: str, task_id: str, attempt_number: int) -> str:
        """Return the stable branch name for one run/task attempt."""
        if not run_branch.strip() or not task_id.strip() or attempt_number < 1:
            raise TaskWorktreeError("run, task, and positive attempt are required")
        run_token = cls._slug(run_branch)[-32:]
        task_token = cls._slug(task_id)[:48]
        return f"factory/tasks/{run_token}/{task_token}/attempt-{attempt_number}"

    def create(
        self,
        run_branch: str,
        task_id: str,
        *,
        attempt_number: int,
    ) -> TaskWorktree:
        """Create a new task branch from the current run-branch commit."""
        if not run_branch.strip() or not task_id.strip():
            raise TaskWorktreeError("run branch and task identifier are required")
        if attempt_number < 1:
            raise TaskWorktreeError("attempt number must be positive")

        self._git(
            self.repository,
            "check-ref-format",
            f"refs/heads/{run_branch}",
        )
        run_token = self._slug(run_branch)[-32:]
        task_token = self._slug(task_id)[:48]
        branch_name = self.branch_name(run_branch, task_id, attempt_number)
        self._git(
            self.repository,
            "check-ref-format",
            f"refs/heads/{branch_name}",
        )
        start_result = self._git(
            self.repository,
            "rev-parse",
            "--verify",
            f"refs/heads/{run_branch}^{{commit}}",
        )
        starting_commit = start_result.stdout.strip()
        path = self.worktree_root / run_token / f"{task_token}-attempt-{attempt_number}"
        if path.exists() or path.is_symlink():
            raise TaskWorktreeError(f"task worktree path already exists: {path}")

        path.parent.mkdir(parents=True, exist_ok=True)
        self._git(
            self.repository,
            "worktree",
            "add",
            "-b",
            branch_name,
            str(path),
            starting_commit,
        )
        return TaskWorktree(
            repository=self.repository,
            worktree_root=self.worktree_root,
            run_branch=run_branch,
            task_id=task_id,
            attempt_number=attempt_number,
            branch_name=branch_name,
            path=path,
            starting_commit=starting_commit,
        )

    def cleanup(self, task_worktree: TaskWorktree) -> None:
        """Remove a clean task checkout while preserving its task branch."""
        if task_worktree.repository != self.repository:
            raise TaskWorktreeError("task worktree belongs to another repository")
        if task_worktree.worktree_root != self.worktree_root:
            raise TaskWorktreeError(
                "task worktree is outside the managed worktree root"
            )
        run_token = self._slug(task_worktree.run_branch)[-32:]
        task_token = self._slug(task_worktree.task_id)[:48]
        expected_branch = (
            f"factory/tasks/{run_token}/{task_token}/"
            f"attempt-{task_worktree.attempt_number}"
        )
        expected_path = (
            self.worktree_root
            / run_token
            / f"{task_token}-attempt-{task_worktree.attempt_number}"
        )
        path = task_worktree.path.resolve()
        if (
            task_worktree.branch_name != expected_branch
            or path != expected_path
            or not path.is_relative_to(self.worktree_root)
        ):
            raise TaskWorktreeError(
                "refusing to remove a path outside the task worktree root"
            )
        if not path.is_dir():
            raise TaskWorktreeError(f"task worktree does not exist: {path}")

        result = subprocess.run(
            ["git", "worktree", "remove", str(path)],
            cwd=self.repository,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            details = result.stderr.strip() or result.stdout.strip()
            if details:
                raise TaskWorktreeError(
                    f"cannot remove worktree with uncommitted changes: {details}"
                )
            raise TaskWorktreeError("could not remove task worktree")
