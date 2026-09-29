import subprocess
from pathlib import Path

import pytest

from cronos_ai.attention import HumanActionProcessor, HumanAttentionQueue
from cronos_ai.models import (
    ControlAction,
    ControlActionType,
    OpenSpecPlan,
    PlanTask,
    TaskRecord,
    TaskState,
    WorkRequest,
)
from cronos_ai.storage import FactoryStore
from cronos_ai.task_integration import (
    IntegrationError,
    MergeStatus,
    TaskIntegrator,
)
from cronos_ai.worktrees import TaskWorktree, TaskWorktreeManager


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def commit(repository: Path, message: str) -> None:
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "user.name=factory-test",
            "-c",
            "user.email=factory-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        check=True,
    )


def create_run(tmp_path: Path, task_plan: tuple[PlanTask, ...]):
    repository = tmp_path / "project"
    repository.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", "--initial-branch=main", str(repository)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(repository, "config", "user.name", "factory-test")
    git(repository, "config", "user.email", "factory-test@example.invalid")
    (repository / "README.md").write_text("base\n")
    git(repository, "add", "README.md")
    commit(repository, "initial")
    run_branch = "factory/run/run-1"
    git(repository, "branch", run_branch)
    run_worktree = tmp_path / "run-worktree"
    git(repository, "worktree", "add", str(run_worktree), run_branch)
    request = WorkRequest(
        request_id="request-1",
        description="Integrate task results",
        repo_path=repository,
    )
    plan = OpenSpecPlan(change_name="integrate-task-results", tasks=task_plan)
    store = FactoryStore(tmp_path / "factory.sqlite3")
    initial_tasks = tuple(
        TaskRecord(
            task_id=task.task_id,
            state=(TaskState.READY if not task.depends_on else TaskState.QUEUED),
        )
        for task in task_plan
    )
    store.create_run("run-1", request, plan, initial_tasks)
    manager = TaskWorktreeManager(repository, tmp_path / "task-worktrees")
    integrator = TaskIntegrator(
        "run-1",
        run_branch,
        run_worktree,
        store,
        task_worktree_root=manager.worktree_root,
    )
    return repository, run_branch, run_worktree, manager, integrator, store


def create_task_commit(
    manager: TaskWorktreeManager,
    run_branch: str,
    task_id: str,
    filename: str,
    content: str,
) -> TaskWorktree:
    task_worktree = manager.create(run_branch, task_id, attempt_number=1)
    (task_worktree.path / filename).write_text(content)
    git(task_worktree.path, "add", filename)
    commit(task_worktree.path, f"implement {task_id}")
    return task_worktree


def test_tasks_merge_only_after_dependencies_are_complete(tmp_path: Path) -> None:
    repository, run_branch, run_worktree, manager, integrator, store = create_run(
        tmp_path,
        (
            PlanTask(task_id="first", description="First task"),
            PlanTask(
                task_id="second",
                description="Second task",
                depends_on=("first",),
            ),
        ),
    )
    first = create_task_commit(
        manager,
        run_branch,
        "first",
        "first.txt",
        "first result\n",
    )
    second = create_task_commit(
        manager,
        run_branch,
        "second",
        "second.txt",
        "second result\n",
    )

    with pytest.raises(IntegrationError, match="dependency.*not complete"):
        integrator.integrate(second)

    assert not (run_worktree / "second.txt").exists()
    first_result = integrator.integrate(first)
    assert first_result.status is MergeStatus.MERGED
    assert (
        git(
            run_worktree,
            "show",
            "-s",
            "--format=%an <%ae>",
            first_result.merge_commit,
        )
        == "Cronos AI <cronos-ai@localhost>"
    )
    assert store.get_task("run-1", "first").state is TaskState.REVIEW
    store.save_task(
        "run-1",
        TaskRecord(task_id="first", state=TaskState.DONE),
    )
    repeated_result = integrator.integrate(first)
    assert repeated_result.status is MergeStatus.ALREADY_INTEGRATED
    assert store.get_task("run-1", "first").state is TaskState.DONE

    second_result = integrator.integrate(second)

    assert second_result.status is MergeStatus.MERGED
    assert (run_worktree / "first.txt").read_text() == "first result\n"
    assert (run_worktree / "second.txt").read_text() == "second result\n"
    assert store.get_task("run-1", "second").state is TaskState.REVIEW
    assert git(repository, "branch", "--show-current") == "main"
    assert git(repository, "status", "--porcelain=v1") == ""
    assert git(run_worktree, "status", "--porcelain=v1") == ""
    store.close()


def test_task_branch_from_another_run_is_rejected(tmp_path: Path) -> None:
    repository, run_branch, run_worktree, manager, integrator, store = create_run(
        tmp_path,
        (PlanTask(task_id="first", description="First task"),),
    )
    other_run_branch = "factory/run/another"
    git(repository, "branch", other_run_branch)
    foreign_task = create_task_commit(
        manager,
        other_run_branch,
        "first",
        "foreign.txt",
        "Not part of the current run\n",
    )

    with pytest.raises(IntegrationError, match="different run branch"):
        integrator.integrate(foreign_task)

    assert not (run_worktree / "foreign.txt").exists()
    assert git(run_worktree, "status", "--porcelain=v1") == ""
    store.close()


def test_merge_conflict_blocks_task_and_preserves_both_branch_results(
    tmp_path: Path,
) -> None:
    repository, run_branch, run_worktree, manager, integrator, store = create_run(
        tmp_path,
        (PlanTask(task_id="conflicting-task", description="Edit the shared file"),),
    )
    task_worktree = manager.create(
        run_branch,
        "conflicting-task",
        attempt_number=1,
    )
    (task_worktree.path / "README.md").write_text("task branch result\n")
    git(task_worktree.path, "add", "README.md")
    commit(task_worktree.path, "task branch edit")
    (run_worktree / "README.md").write_text("run branch result\n")
    git(run_worktree, "add", "README.md")
    commit(run_worktree, "run branch edit")
    run_commit = git(run_worktree, "rev-parse", "HEAD")
    task_commit = git(task_worktree.path, "rev-parse", "HEAD")

    result = integrator.integrate(task_worktree)

    assert result.status is MergeStatus.BLOCKED
    assert result.conflicting_paths == ("README.md",)
    blocked = store.get_task("run-1", "conflicting-task")
    assert blocked is not None
    assert blocked.state is TaskState.BLOCKED
    assert "README.md" in blocked.reason
    assert git(run_worktree, "rev-parse", "HEAD") == run_commit
    assert git(task_worktree.path, "rev-parse", "HEAD") == task_commit
    assert git(run_worktree, "show", f"{run_branch}:README.md") == "run branch result"
    assert (
        git(
            task_worktree.path,
            "show",
            f"{task_worktree.branch_name}:README.md",
        )
        == "task branch result"
    )
    assert "<<<<<<<" in (run_worktree / "README.md").read_text()
    assert git(repository, "branch", "--show-current") == "main"
    assert git(repository, "status", "--porcelain=v1") == ""
    context = store.get_run_context("run-1")
    assert context is not None
    assert context.branch_name == run_branch
    assert context.run_worktree_path == run_worktree.resolve()
    assert context.task_worktree_root == manager.worktree_root
    store.close()


def test_human_resolves_blocked_merge_through_audited_action(tmp_path: Path) -> None:
    repository, run_branch, run_worktree, manager, integrator, store = create_run(
        tmp_path,
        (PlanTask(task_id="conflicting-task", description="Edit shared file"),),
    )
    task_worktree = manager.create(run_branch, "conflicting-task", attempt_number=1)
    (task_worktree.path / "README.md").write_text("task branch result\n")
    git(task_worktree.path, "add", "README.md")
    commit(task_worktree.path, "task branch edit")
    (run_worktree / "README.md").write_text("run branch result\n")
    git(run_worktree, "add", "README.md")
    commit(run_worktree, "run branch edit")
    integrator.integrate(task_worktree)

    attention = HumanAttentionQueue(store).list_items("run-1")
    conflict_item = next(
        item for item in attention if item.item_id == "run-1:conflicting-task"
    )
    assert conflict_item.available_actions == (ControlActionType.RESOLVE_CONFLICT,)

    (run_worktree / "README.md").write_text("human-resolved result\n")
    git(run_worktree, "add", "README.md")
    store.enqueue_control_action(
        ControlAction(
            action_id="resolve-conflict-1",
            action_type=ControlActionType.RESOLVE_CONFLICT,
            target_id="conflicting-task",
            run_id="run-1",
            actor="operator-1",
            payload={"resolution": "Keep the reconciled implementation"},
        )
    )

    result = HumanActionProcessor(store).process_pending()[0]
    resolved = store.get_task("run-1", "conflicting-task")

    assert result.status.value == "done"
    assert resolved is not None and resolved.state is TaskState.REVIEW
    assert git(run_worktree, "status", "--porcelain=v1") == ""
    assert git(run_worktree, "show", "HEAD:README.md") == "human-resolved result"
    assert git(repository, "branch", "--show-current") == "main"
    store.close()
