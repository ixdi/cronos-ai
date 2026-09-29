"""Explicit OpenSpec initialization for selected Git repositories."""

from __future__ import annotations

import subprocess
from pathlib import Path


class RepositoryInitializationError(RuntimeError):
    """Raised when a repository cannot be safely initialized."""


def initialize_repository(repo_path: Path) -> Path:
    """Initialize OpenSpec at an explicitly selected Git repository root."""
    try:
        repository = repo_path.resolve(strict=True)
    except OSError as error:
        raise RepositoryInitializationError(
            f"repository path does not exist: {repo_path}"
        ) from error

    if not repository.is_dir():
        raise RepositoryInitializationError(
            f"repository path is not a directory: {repository}"
        )

    try:
        result = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=repository,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RepositoryInitializationError("Git is unavailable") from error

    if result.returncode != 0:
        raise RepositoryInitializationError(
            f"path is not inside a Git working tree: {repository}"
        )

    git_root = Path(result.stdout.strip()).resolve()
    if git_root != repository:
        raise RepositoryInitializationError(
            f"--repo must point to the Git repository root: {git_root}"
        )

    openspec_root = repository / "openspec"
    config_files = (openspec_root / "config.yaml", openspec_root / "config.yml")
    if any(config_file.is_file() for config_file in config_files):
        return repository
    if openspec_root.exists():
        raise RepositoryInitializationError(
            f"an uninitialized openspec directory already exists: {openspec_root}"
        )

    command = ["openspec", "init", str(repository), "--tools", "none"]
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RepositoryInitializationError("OpenSpec CLI is unavailable") from error

    if result.returncode != 0:
        details = result.stderr.strip() or result.stdout.strip()
        message = f"OpenSpec initialization failed with exit code {result.returncode}"
        if details:
            message = f"{message}: {details}"
        raise RepositoryInitializationError(message)

    if not any(config_file.is_file() for config_file in config_files):
        raise RepositoryInitializationError(
            "OpenSpec init completed without creating an openspec config file"
        )
    return repository
