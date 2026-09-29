"""Dependency-aware Git task integration and conflict reporting."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path

from cronos_ai.approval import ApprovalError, plan_fingerprint
from cronos_ai.models import RunContext, TaskState
from cronos_ai.storage import FactoryStore
from cronos_ai.worktrees import TaskWorktree, TaskWorktreeManager


class IntegrationError(RuntimeError):
    """Raised when task results cannot be safely integrated."""


class MergeStatus(StrEnum):
    MERGED = "merged"
    ALREADY_INTEGRATED = "already-integrated"
    BLOCKED = "blocked"


@dataclass(frozen=True)
class MergeResult:
    """Outcome of one task branch integration attempt."""

    task_id: str
    status: MergeStatus
    merge_commit: str | None = None
    conflicting_paths: tuple[str, ...] = ()
    reason: str | None = None


class TaskIntegrator:
    """Merge verified task branches in dependency order into a run branch."""

    def __init__(
        self,
        run_id: str,
        run_branch: str,
        run_worktree: Path,
        store: FactoryStore,
        *,
        starting_commit: str | None = None,
        task_worktree_root: Path | None = None,
    ) -> None:
        try:
            self.run_worktree = run_worktree.resolve(strict=True)
        except OSError as error:
            raise IntegrationError("run branch worktree does not exist") from error
        self.run_id = run_id
        self.run_branch = run_branch
        self.store = store
        run_root = self._git(
            self.run_worktree,
            "rev-parse",
            "--show-toplevel",
        ).stdout.strip()
        if Path(run_root).resolve() != self.run_worktree:
            raise IntegrationError("run worktree path must be its Git root")
        common_dir = Path(
            self._git(
                self.run_worktree,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ).stdout.strip()
        ).resolve()
        run = self.store.get_run(self.run_id)
        if run is None:
            raise IntegrationError(f"run does not exist: {self.run_id}")
        request, _ = run
        try:
            target_common_dir = Path(
                self._git(
                    request.repo_path,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ).stdout.strip()
            ).resolve()
        except IntegrationError as error:
            raise IntegrationError("target repository is unavailable") from error
        if target_common_dir != common_dir:
            raise IntegrationError(
                "run worktree does not belong to the target repository"
            )
        self.repository = str(common_dir.parent)
        current_branch = self._git(
            self.run_worktree,
            "branch",
            "--show-current",
        ).stdout.strip()
        if current_branch != self.run_branch:
            raise IntegrationError("run worktree is not on the configured run branch")
        self._persist_run_context(starting_commit, task_worktree_root)

    def _persist_run_context(
        self,
        starting_commit: str | None,
        task_worktree_root: Path | None,
    ) -> None:
        run = self.store.get_run(self.run_id)
        if run is None:
            raise IntegrationError(f"run does not exist: {self.run_id}")
        _, plan = run
        current = self.store.get_run_context(self.run_id)
        plan_hash = current.plan_hash if current is not None else None
        if plan_hash is None:
            change_dir = self.run_worktree / "openspec" / "changes" / plan.change_name
            if change_dir.is_dir():
                try:
                    plan_hash = plan_fingerprint(change_dir)
                except ApprovalError as error:
                    raise IntegrationError(
                        "could not fingerprint the run's OpenSpec plan"
                    ) from error
        context = RunContext(
            run_id=self.run_id,
            branch_name=self.run_branch,
            run_worktree_path=self.run_worktree,
            starting_commit=(
                starting_commit
                if starting_commit is not None
                else current.starting_commit
                if current is not None
                else None
            ),
            task_worktree_root=(
                task_worktree_root.resolve()
                if task_worktree_root is not None
                else current.task_worktree_root
                if current is not None
                else None
            ),
            plan_hash=plan_hash,
            approval_required=(
                current.approval_required if current is not None else False
            ),
            triage=current.triage if current is not None else None,
        )
        self.store.save_run_context(context)

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
            raise IntegrationError("Git is unavailable") from error
        if result.returncode != 0:
            details = result.stderr.strip() or result.stdout.strip()
            message = f"Git command failed: {' '.join(args)}"
            if details:
                message = f"{message}: {details}"
            raise IntegrationError(message)
        return result

    def _require_clean_worktree(self, path: Path, label: str) -> None:
        result = self._git(
            path,
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        )
        if result.stdout.strip():
            raise IntegrationError(f"{label} worktree has uncommitted changes")

    def _record_state(
        self,
        task_id: str,
        state: TaskState,
        *,
        reason: str | None = None,
    ) -> None:
        current = self.store.get_task(self.run_id, task_id)
        if current is None:
            raise IntegrationError(
                f"task is missing from the execution ledger: {task_id}"
            )
        if state is TaskState.REVIEW and current.state in (
            TaskState.DONE,
            TaskState.FAILED,
        ):
            return
        updated = current.model_copy(
            update={
                "state": state,
                "reason": reason,
                "worker_id": None,
            }
        )
        self.store.save_task(self.run_id, updated)

    def integrate(self, task_worktree: TaskWorktree) -> MergeResult:
        """Merge a clean task branch or persist a reason-bearing conflict."""
        run = self.store.get_run(self.run_id)
        if run is None:
            raise IntegrationError(f"run does not exist: {self.run_id}")
        _, plan = run
        plan_task = next(
            (task for task in plan.tasks if task.task_id == task_worktree.task_id),
            None,
        )
        if plan_task is None:
            raise IntegrationError(
                f"task is not part of the run's OpenSpec plan: {task_worktree.task_id}"
            )
        for dependency in plan_task.depends_on:
            dependency_record = self.store.get_task(self.run_id, dependency)
            if dependency_record is None or dependency_record.state not in (
                TaskState.REVIEW,
                TaskState.DONE,
            ):
                raise IntegrationError(
                    f"dependency {dependency!r} is not complete for task "
                    f"{plan_task.task_id!r}"
                )

        task_path = task_worktree.path.resolve(strict=True)
        if task_worktree.run_branch != self.run_branch:
            raise IntegrationError("task worktree belongs to a different run branch")
        expected_branch = TaskWorktreeManager.branch_name(
            task_worktree.run_branch,
            task_worktree.task_id,
            task_worktree.attempt_number,
        )
        if task_worktree.branch_name != expected_branch:
            raise IntegrationError(
                "task branch does not match its run and task identity"
            )
        if task_worktree.repository != Path(self.repository).resolve():
            raise IntegrationError("task worktree belongs to another Git repository")
        if (
            Path(
                self._git(task_path, "rev-parse", "--show-toplevel").stdout.strip()
            ).resolve()
            != task_path
        ):
            raise IntegrationError("task worktree path must be its Git root")
        task_branch = self._git(
            task_path,
            "branch",
            "--show-current",
        ).stdout.strip()
        if task_branch != task_worktree.branch_name:
            raise IntegrationError("task worktree is not on its assigned branch")

        self._require_clean_worktree(self.run_worktree, "run branch")
        self._require_clean_worktree(task_path, "task")
        task_ref = f"refs/heads/{task_worktree.branch_name}"
        self._git(Path(self.repository), "check-ref-format", task_ref)

        ancestor = subprocess.run(
            [
                "git",
                "merge-base",
                "--is-ancestor",
                task_ref,
                f"refs/heads/{self.run_branch}",
            ],
            cwd=self.run_worktree,
            capture_output=True,
            text=True,
            check=False,
        )
        if ancestor.returncode == 0:
            commit = self._git(
                self.run_worktree,
                "rev-parse",
                "HEAD",
            ).stdout.strip()
            self._record_state(task_worktree.task_id, TaskState.REVIEW)
            return MergeResult(
                task_id=task_worktree.task_id,
                status=MergeStatus.ALREADY_INTEGRATED,
                merge_commit=commit,
            )
        if ancestor.returncode != 1:
            raise IntegrationError("could not compare task and run branch history")

        merge = subprocess.run(
            [
                "git",
                "-c",
                "user.name=Cronos AI",
                "-c",
                "user.email=cronos-ai@localhost",
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-edit",
                "--no-ff",
                "--no-stat",
                task_ref,
            ],
            cwd=self.run_worktree,
            capture_output=True,
            text=True,
            check=False,
        )
        if merge.returncode == 0:
            commit = self._git(
                self.run_worktree,
                "rev-parse",
                "HEAD",
            ).stdout.strip()
            self._record_state(task_worktree.task_id, TaskState.REVIEW)
            return MergeResult(
                task_id=task_worktree.task_id,
                status=MergeStatus.MERGED,
                merge_commit=commit,
            )

        conflicts = self._git(
            self.run_worktree,
            "diff",
            "--name-only",
            "--diff-filter=U",
        ).stdout.splitlines()
        if conflicts:
            conflict_paths = tuple(sorted(set(conflicts)))
            reason = "Merge conflict requires human resolution: " + ", ".join(
                conflict_paths
            )
            self._record_state(
                task_worktree.task_id,
                TaskState.BLOCKED,
                reason=reason,
            )
            return MergeResult(
                task_id=task_worktree.task_id,
                status=MergeStatus.BLOCKED,
                conflicting_paths=conflict_paths,
                reason=reason,
            )

        self._git(self.run_worktree, "merge", "--abort")
        details = merge.stderr.strip() or merge.stdout.strip()
        raise IntegrationError(
            f"Git could not merge task branch {task_worktree.branch_name}: {details}"
        )

    def resolve_conflict(self, task_id: str, resolution: str) -> str:
        """Continue a human-resolved task merge in the persisted run worktree."""
        task = self.store.get_task(self.run_id, task_id)
        if task is None:
            raise IntegrationError("target task does not exist in this run")
        if (
            task.state is TaskState.REVIEW
            and task.reason is not None
            and resolution.strip() in task.reason
        ):
            return self._git(self.run_worktree, "rev-parse", "HEAD").stdout.strip()
        if task.state is not TaskState.BLOCKED:
            raise IntegrationError("target task is not blocked in this run")
        if task.reason is None or "conflict" not in task.reason.casefold():
            raise IntegrationError(
                "target task is not blocked by an integration conflict"
            )
        if not resolution.strip():
            raise IntegrationError("human conflict resolution must include a note")

        merge_head = subprocess.run(
            ["git", "rev-parse", "--verify", "-q", "MERGE_HEAD"],
            cwd=self.run_worktree,
            capture_output=True,
            text=True,
            check=False,
        )
        if merge_head.returncode != 0:
            raise IntegrationError("run worktree has no active merge to continue")
        conflicts = self._git(
            self.run_worktree,
            "diff",
            "--name-only",
            "--diff-filter=U",
        ).stdout.splitlines()
        if conflicts:
            raise IntegrationError(
                "run worktree still has unresolved paths: "
                + ", ".join(sorted(set(conflicts)))
            )
        unstaged = self._git(
            self.run_worktree, "diff", "--name-only"
        ).stdout.splitlines()
        untracked = self._git(
            self.run_worktree, "ls-files", "--others", "--exclude-standard"
        ).stdout.splitlines()
        if unstaged or untracked:
            paths = sorted(set(unstaged + untracked))
            raise IntegrationError(
                "stage resolved changes and remove unrelated untracked paths: "
                + ", ".join(paths)
            )

        result = subprocess.run(
            [
                "git",
                "-c",
                "user.name=Cronos AI",
                "-c",
                "user.email=cronos-ai@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.editor=true",
                "merge",
                "--continue",
            ],
            cwd=self.run_worktree,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            details = result.stderr.strip() or result.stdout.strip()
            raise IntegrationError(f"could not continue resolved merge: {details}")
        commit = self._git(self.run_worktree, "rev-parse", "HEAD").stdout.strip()
        self._record_state(
            task_id,
            TaskState.REVIEW,
            reason=f"Human conflict resolution: {resolution.strip()}",
        )
        return commit
