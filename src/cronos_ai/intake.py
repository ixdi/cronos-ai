"""Safe request intake for explicitly selected target repositories."""

from __future__ import annotations

import subprocess
from pathlib import Path
from uuid import uuid4

from cronos_ai.models import RequestSource, WorkRequest


class IntakeError(ValueError):
    """Raised when a request's target does not meet intake prerequisites."""


def intake_request(
    repo_path: Path | None,
    description: str,
    *,
    source: RequestSource = RequestSource.USER,
    request_id: str | None = None,
) -> WorkRequest:
    """Validate a target without modifying it, then create a request model."""
    if repo_path is None:
        raise IntakeError("an explicit repository path is required")

    try:
        repository = repo_path.resolve(strict=True)
    except OSError as error:
        raise IntakeError(f"repository path does not exist: {repo_path}") from error
    if not repository.is_dir():
        raise IntakeError(f"repository path is not a directory: {repository}")

    try:
        root_result = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise IntakeError("Git is unavailable") from error
    if root_result.returncode != 0:
        raise IntakeError(f"target is not a Git working tree: {repository}")

    git_root = Path(root_result.stdout.strip()).resolve()
    if git_root != repository:
        raise IntakeError(f"repository path must be the Git root: {git_root}")

    openspec_root = repository / "openspec"
    if not (
        (openspec_root / "config.yaml").is_file()
        or (openspec_root / "config.yml").is_file()
    ):
        raise IntakeError(
            f"target requires an initialized OpenSpec root: {openspec_root}"
        )

    try:
        status_result = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise IntakeError("Git is unavailable") from error
    if status_result.returncode != 0:
        raise IntakeError(f"could not inspect Git working tree: {repository}")
    if status_result.stdout.strip():
        raise IntakeError("target repository must have a clean working tree")

    return WorkRequest(
        request_id=request_id or str(uuid4()),
        description=description,
        repo_path=repository,
        source=source,
    )
