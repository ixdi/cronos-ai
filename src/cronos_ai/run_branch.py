"""Isolated run-branch preparation and OpenSpec plan commits."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

from cronos_ai.intake import IntakeError, intake_request
from cronos_ai.models import RunContext, TriageResult, WorkRequest
from cronos_ai.planning import (
    GeneratedOpenSpecChange,
    PlanningError,
    generate_openspec_change,
)


class RunBranchError(RuntimeError):
    """Raised when an isolated run branch cannot be prepared safely."""


@dataclass(frozen=True)
class RunBranch:
    """A dedicated worktree and committed plan for one factory run."""

    branch_name: str
    worktree_path: Path
    starting_commit: str
    plan: GeneratedOpenSpecChange

    def to_context(
        self,
        run_id: str,
        *,
        task_worktree_root: Path | None = None,
        approval_required: bool | None = None,
        triage_result: TriageResult | None = None,
    ) -> RunContext:
        """Create the durable context required for scheduling and recovery."""
        return RunContext(
            run_id=run_id,
            branch_name=self.branch_name,
            run_worktree_path=self.worktree_path.resolve(),
            starting_commit=self.starting_commit,
            task_worktree_root=(
                task_worktree_root.resolve() if task_worktree_root is not None else None
            ),
            plan_hash=self.plan.plan_hash,
            approval_required=(
                triage_result.requires_approval
                if approval_required is None and triage_result is not None
                else bool(approval_required)
            ),
            triage=triage_result,
        )


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
        raise RunBranchError("Git is unavailable") from error
    if result.returncode != 0:
        details = result.stderr.strip() or result.stdout.strip()
        message = f"Git command failed: {' '.join(args)}"
        if details:
            message = f"{message}: {details}"
        raise RunBranchError(message)
    return result


def _branch_name(request: WorkRequest) -> str:
    request_id = re.sub(r"[^a-z0-9]+", "-", request.request_id.casefold()).strip("-")
    return f"factory/run-{request_id[-24:] or 'request'}"


def _cleanup_run_branch(
    repository: Path,
    branch_name: str,
    worktree_path: Path,
) -> None:
    subprocess.run(
        ["git", "worktree", "remove", "--force", str(worktree_path)],
        cwd=repository,
        capture_output=True,
        text=True,
        check=False,
    )
    subprocess.run(
        ["git", "branch", "-D", branch_name],
        cwd=repository,
        capture_output=True,
        text=True,
        check=False,
    )


def create_run_branch_with_plan(
    request: WorkRequest,
    triage: TriageResult,
    worktree_path: Path,
) -> RunBranch:
    """Create a run branch, generate its plan there, and commit the plan."""
    if not triage.can_plan:
        raise RunBranchError("triage outcome does not permit planning")
    if (
        triage.request.request_id != request.request_id
        or triage.request.repo_path.resolve() != request.repo_path.resolve()
    ):
        raise RunBranchError("triage result does not match the submitted request")

    repository = request.repo_path.resolve()
    worktree = worktree_path.resolve()
    if worktree.is_relative_to(repository):
        raise RunBranchError("run worktree must be outside the original checkout")
    if worktree.exists() or worktree.is_symlink():
        raise RunBranchError(f"run worktree path already exists: {worktree}")

    try:
        validated_request = intake_request(request.repo_path, request.description)
    except IntakeError as error:
        raise RunBranchError(str(error)) from error
    repository = validated_request.repo_path

    start_result = _git(repository, "rev-parse", "--verify", "HEAD^{commit}")
    starting_commit = start_result.stdout.strip()
    branch_name = _branch_name(request)
    branch_check = subprocess.run(
        ["git", "show-ref", "--verify", "--quiet", f"refs/heads/{branch_name}"],
        cwd=repository,
        capture_output=True,
        text=True,
        check=False,
    )
    if branch_check.returncode == 0:
        raise RunBranchError(f"run branch already exists: {branch_name}")
    if branch_check.returncode not in (1,):
        raise RunBranchError("could not verify whether the run branch already exists")

    worktree.parent.mkdir(parents=True, exist_ok=True)
    _git(
        repository,
        "worktree",
        "add",
        "-b",
        branch_name,
        str(worktree),
        starting_commit,
    )

    try:
        run_request = request.model_copy(update={"repo_path": worktree})
        run_triage = triage.model_copy(update={"request": run_request})
        try:
            plan = generate_openspec_change(run_request, run_triage)
        except PlanningError as error:
            raise RunBranchError(str(error)) from error

        _git(
            worktree,
            "add",
            "--",
            f"openspec/changes/{plan.change_name}",
        )
        _git(
            worktree,
            "-c",
            "user.name=Cronos AI",
            "-c",
            "user.email=cronos-ai@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            f"chore(openspec): plan {plan.change_name}",
        )
    except Exception as error:
        _cleanup_run_branch(repository, branch_name, worktree)
        if isinstance(error, RunBranchError):
            raise
        raise RunBranchError(f"could not commit the OpenSpec plan: {error}") from error

    return RunBranch(
        branch_name=branch_name,
        worktree_path=worktree,
        starting_commit=starting_commit,
        plan=plan,
    )
