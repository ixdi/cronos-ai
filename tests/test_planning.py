import shutil
import subprocess
from pathlib import Path

import pytest

from cronos_ai.models import TriageOutcome, TriageResult, WorkRequest
from cronos_ai.planning import PlanningError, generate_openspec_change
from cronos_ai.triage import triage_request


def make_request(repository: Path) -> WorkRequest:
    return WorkRequest(
        request_id="request-12345678",
        description="Improve the settings page",
        repo_path=repository,
    )


def make_openspec_root(repository: Path) -> None:
    openspec = repository / "openspec"
    openspec.mkdir(parents=True)
    (openspec / "config.yaml").write_text("schema: spec-driven\n")


def test_actionable_request_generates_concise_change(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    make_openspec_root(repository)
    request = make_request(repository)
    triage = triage_request(
        request,
        TriageOutcome.ACTIONABLE,
        rationale="The request is bounded.",
    )
    commands: list[list[str]] = []

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        if command[1] == "new":
            (repository / "openspec/changes" / command[3]).mkdir(parents=True)
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr("cronos_ai.planning.subprocess.run", run)

    plan = generate_openspec_change(request, triage)

    change_dir = repository / "openspec/changes" / plan.change_name
    assert plan.change_name == "improve-the-settings-page-request-12345678"
    assert plan.change_dir == change_dir
    assert plan.full_plan is False
    assert (change_dir / "proposal.md").is_file()
    assert (change_dir / "specs/requested-outcome/spec.md").is_file()
    assert (change_dir / "design.md").is_file()
    assert (change_dir / "tasks.md").is_file()
    assert len(plan.tasks) == 2
    assert [command[1] for command in commands] == ["new", "validate"]
    assert all(command[0] == "openspec" for command in commands)


def test_specifications_required_request_generates_full_approval_draft(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    make_openspec_root(repository)
    request = make_request(repository)
    triage = triage_request(
        request,
        TriageOutcome.SPECS_REQUIRED,
        rationale="Scope needs product and technical detail.",
    )

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[1] == "new":
            (repository / "openspec/changes" / command[3]).mkdir(parents=True)
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr("cronos_ai.planning.subprocess.run", run)

    plan = generate_openspec_change(request, triage)
    tasks = (plan.change_dir / "tasks.md").read_text()

    assert plan.full_plan is True
    assert plan.can_dispatch_directly is False
    assert "human approval" in tasks.lower()
    assert len(plan.tasks) > 2


def test_non_plannable_triage_does_not_create_a_change(tmp_path: Path) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    make_openspec_root(repository)
    request = make_request(repository)
    triage = triage_request(
        request,
        TriageOutcome.CLARIFICATION_REQUIRED,
        rationale="A behavior decision is missing.",
        questions=("Which behavior should be used?",),
    )

    with pytest.raises(PlanningError, match="cannot be planned"):
        generate_openspec_change(request, triage)

    assert not (repository / "openspec/changes").exists()


def test_triage_must_belong_to_the_request(tmp_path: Path) -> None:
    repository = tmp_path / "project"
    repository.mkdir()
    make_openspec_root(repository)
    request = make_request(repository)
    other_request = request.model_copy(
        update={
            "request_id": "another-request",
            "triage_outcome": TriageOutcome.ACTIONABLE,
        }
    )
    triage = TriageResult(
        request=other_request,
        outcome=TriageOutcome.ACTIONABLE,
        rationale="Clear request.",
    )

    with pytest.raises(PlanningError, match="does not match"):
        generate_openspec_change(request, triage)


def create_git_repository(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return path.resolve()


@pytest.mark.skipif(shutil.which("openspec") is None, reason="OpenSpec CLI unavailable")
def test_created_change_passes_openspec_validation(tmp_path: Path) -> None:
    repository = create_git_repository(tmp_path / "project")
    subprocess.run(
        ["openspec", "init", str(repository), "--tools", "none"],
        check=True,
        capture_output=True,
        text=True,
    )
    request = make_request(repository)
    triage = triage_request(
        request,
        TriageOutcome.ACTIONABLE,
        rationale="The request is bounded.",
    )

    plan = generate_openspec_change(request, triage)
    validation = subprocess.run(
        ["openspec", "validate", plan.change_name, "--no-interactive"],
        cwd=repository,
        capture_output=True,
        text=True,
        check=False,
    )

    assert validation.returncode == 0, validation.stdout + validation.stderr


@pytest.mark.skipif(shutil.which("openspec") is None, reason="OpenSpec CLI unavailable")
def test_detailed_generated_change_passes_openspec_validation(tmp_path: Path) -> None:
    repository = create_git_repository(tmp_path / "project")
    subprocess.run(
        ["openspec", "init", str(repository), "--tools", "none"],
        check=True,
        capture_output=True,
        text=True,
    )
    request = make_request(repository)
    triage = triage_request(
        request,
        TriageOutcome.SPECS_REQUIRED,
        rationale="The request needs detailed product and technical specifications.",
    )

    plan = generate_openspec_change(request, triage)
    validation = subprocess.run(
        ["openspec", "validate", plan.change_name, "--no-interactive"],
        cwd=repository,
        capture_output=True,
        text=True,
        check=False,
    )

    assert plan.full_plan is True
    assert validation.returncode == 0, validation.stdout + validation.stderr
