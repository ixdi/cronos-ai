#![cfg(unix)]

//! Herdr agent lifecycle against a fake `herdr`: sticky cancellation, time limits, pane cleanup,
//! attempt-scoped agent names, stale-agent cleanup, and the per-task step lock.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use cronos_ai::{
    agent_profiles::AgentProfileCatalog,
    integrations::{GitAdapter, ToolCommands},
    model::{
        AgentStatus, ApprovalDecision, ApprovalGate, Priority, ProjectId, Task, TaskStatus,
        WorkflowStep,
    },
    project::{ProjectError, ProjectManager},
    workflow::{
        AgentAdapter, AgentPlan, HerdrAgentAdapter, WorkflowEngine, WorkflowError,
        cancel_interrupted_agents,
    },
};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Fake Herdr that logs every call. Agents whose Herdr name contains `-fail-` time out on prompt;
/// every other agent writes a success result.
fn fake_herdr(path: &Path, log: &Path) {
    let log = format!("'{}'", log.to_string_lossy().replace('\'', "'\\''"));
    let script = format!(
        r##"#!/bin/sh
HERDR_LOG={log}
printf '%s\0' "$@" > "${{HERDR_LOG}}.$$"
if [ "$1" = "pane" ] && [ "$2" = "split" ]; then
  printf '{{"result":{{"pane":{{"pane_id":"w1:p%s"}}}}}}' "$$"
elif [ "$1" = "agent" ] && [ "$2" = "prompt" ]; then
  case "$3" in
    *-fail-*) echo "timed out waiting for agent" >&2; exit 1 ;;
  esac
  result=$(printf '%s\n' "$4" | sed -n 's/.*write your result as JSON to `\([^`]*\)`.*/\1/p')
  printf '{{"status":"success","summary":"done"}}' > "$result"
elif [ "$1" = "agent" ] && [ "$2" = "get" ]; then
  printf '{{"result":{{"agent":{{"status":"done","pane_id":"w1:stale"}}}}}}'
fi
"##
    );
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

fn logged_calls(log: &Path) -> Vec<Vec<String>> {
    let prefix = format!("{}.", log.file_name().unwrap().to_string_lossy());
    fs::read_dir(log.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| {
            fs::read(entry.path())
                .unwrap()
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
                .collect()
        })
        .collect()
}

fn calls_starting_with(log: &Path, prefix: &[&str]) -> Vec<Vec<String>> {
    logged_calls(log)
        .into_iter()
        .filter(|args| args.len() >= prefix.len() && args.iter().zip(prefix).all(|(a, b)| a == b))
        .collect()
}

struct Fixture {
    _temp: tempfile::TempDir,
    data: std::path::PathBuf,
    log: std::path::PathBuf,
    tools: ToolCommands,
    manager: ProjectManager,
    project_id: ProjectId,
    task: Task,
}

/// A task waiting at code review with a real worktree.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("openspec")).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Cronos E2E"]);
    git(
        &repository,
        &["config", "user.email", "cronos-e2e@example.invalid"],
    );
    fs::write(repository.join("openspec/config.yaml"), "schema: x\n").unwrap();
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-m", "initial"]);

    let data = temp.path().join("data");
    let manager = ProjectManager::new(&data).unwrap();
    let git_adapter = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "Herdr".to_owned(),
            None,
            Some(&repository),
            Some(vec!["true".to_owned()]),
            &git_adapter,
        )
        .unwrap();
    let task = manager
        .create_task(
            project.id,
            "Review".to_owned(),
            None,
            Priority::High,
            Vec::new(),
        )
        .unwrap();
    manager.start_next_task(project.id).unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git_adapter)
        .unwrap();
    manager
        .complete_workflow_step(project.id, task.id, WorkflowStep::SpecificationGeneration)
        .unwrap();
    manager
        .record_approval(
            project.id,
            task.id,
            ApprovalGate::Specifications,
            ApprovalDecision::Approved,
            None,
        )
        .unwrap();
    let task = manager
        .complete_workflow_step(project.id, task.id, WorkflowStep::CodeGeneration)
        .unwrap();

    let herdr = temp.path().join("fake-herdr");
    let log = temp.path().join("herdr.log");
    fake_herdr(&herdr, &log);
    let tools = ToolCommands::with_paths(herdr.clone(), herdr.clone(), herdr.clone(), "git".into());
    Fixture {
        _temp: temp,
        data,
        log,
        tools,
        manager,
        project_id: project.id,
        task,
    }
}

fn plan(name: &str, worktree: &Path, step: WorkflowStep) -> AgentPlan {
    AgentPlan {
        name: name.to_owned(),
        prompt: format!(
            "Role {name}.\nBefore you finish, write your result as JSON to `{}`: {{}}",
            cronos_ai::workflow::agent_result_path(worktree, step, name).display()
        ),
        profile_id: None,
    }
}

fn adapter(fixture: &Fixture) -> HerdrAgentAdapter {
    HerdrAgentAdapter::with_tools_and_profiles(
        fixture.tools.clone(),
        AgentProfileCatalog::default(),
    )
}

#[test]
fn a_cancelled_agent_never_starts_even_if_its_launch_had_not_begun() {
    let fixture = fixture();
    let step = WorkflowStep::CodeReview;
    let worktree = Path::new(fixture.task.worktree.as_deref().unwrap()).to_path_buf();
    let adapter = adapter(&fixture);
    let late = plan("late", &worktree, step);

    adapter.begin_step();
    adapter.cancel(&fixture.task, step, &late);
    let error = adapter.run(&fixture.task, step, &late).unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    assert!(calls_starting_with(&fixture.log, &["agent", "start"]).is_empty());
    assert!(calls_starting_with(&fixture.log, &["pane", "split"]).is_empty());

    // A new step clears earlier cancellations.
    adapter.begin_step();
    let report = adapter.run(&fixture.task, step, &late).unwrap();
    assert_eq!(report.summary, "done");
    assert_eq!(
        calls_starting_with(&fixture.log, &["agent", "start"]).len(),
        1
    );
    assert_eq!(
        calls_starting_with(&fixture.log, &["pane", "close"]).len(),
        1
    );
}

#[test]
fn an_agent_that_exceeds_its_limit_fails_and_its_pane_is_closed() {
    let fixture = fixture();
    let step = WorkflowStep::CodeReview;
    let worktree = Path::new(fixture.task.worktree.as_deref().unwrap()).to_path_buf();
    let adapter = adapter(&fixture);

    let error = adapter
        .run(&fixture.task, step, &plan("fail", &worktree, step))
        .unwrap_err();
    assert!(error.contains("exceeded its 30-minute limit"), "{error}");
    assert_eq!(
        calls_starting_with(&fixture.log, &["agent", "send-keys"]).len(),
        1
    );
    let split = calls_starting_with(&fixture.log, &["pane", "split"]);
    let closed = calls_starting_with(&fixture.log, &["pane", "close"]);
    assert_eq!(split.len(), 1);
    assert_eq!(closed.len(), 1);
    assert!(closed[0][2].starts_with("w1:p"));
}

#[test]
fn a_failed_agent_fails_the_step_and_marks_the_task_error() {
    let fixture = fixture();
    let step = WorkflowStep::CodeReview;
    let worktree = Path::new(fixture.task.worktree.as_deref().unwrap()).to_path_buf();
    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&fixture.data).unwrap(),
        adapter(&fixture),
        AgentProfileCatalog::default(),
        fixture.tools.clone(),
    );
    let result = engine.run_step(
        fixture.project_id,
        fixture.task.id,
        step,
        vec![plan("fail", &worktree, step), plan("good", &worktree, step)],
    );
    assert!(matches!(result, Err(WorkflowError::AgentFailed { .. })));
    let stored = fixture
        .manager
        .get_task(fixture.project_id, fixture.task.id)
        .unwrap();
    assert_eq!(stored.status, TaskStatus::Error);
    // Every pane the step opened is closed again.
    assert_eq!(
        calls_starting_with(&fixture.log, &["pane", "split"]).len(),
        calls_starting_with(&fixture.log, &["pane", "close"]).len()
    );
}

#[test]
fn agent_names_differ_between_attempts() {
    let fixture = fixture();
    let step = WorkflowStep::CodeReview;
    let worktree = Path::new(fixture.task.worktree.as_deref().unwrap()).to_path_buf();
    let adapter = adapter(&fixture);
    let reviewer = plan("reviewer", &worktree, step);
    adapter.run(&fixture.task, step, &reviewer).unwrap();
    let mut next_attempt = fixture.task.clone();
    next_attempt.attempt += 1;
    adapter.run(&next_attempt, step, &reviewer).unwrap();

    let names = calls_starting_with(&fixture.log, &["agent", "start"])
        .into_iter()
        .map(|args| args[2].clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|name| name.len() <= 32));
}

#[test]
fn recovery_waits_for_a_running_step_then_stops_stale_agents() {
    let fixture = fixture();
    let (project_id, task_id) = (fixture.project_id, fixture.task.id);
    fixture
        .manager
        .record_agent_activity(
            project_id,
            task_id,
            "ghost".to_owned(),
            "code-review".to_owned(),
            AgentStatus::Starting,
            None,
        )
        .unwrap();

    // Another process is still running a step for this task.
    let lock = fixture.manager.lock_task(task_id).unwrap();
    assert!(matches!(
        cancel_interrupted_agents(&fixture.manager, &fixture.tools, project_id, task_id),
        Err(WorkflowError::Project(ProjectError::TaskBusy))
    ));
    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&fixture.data).unwrap(),
        adapter(&fixture),
        AgentProfileCatalog::default(),
        fixture.tools.clone(),
    );
    assert!(matches!(
        engine.run_step(
            project_id,
            task_id,
            WorkflowStep::CodeReview,
            vec![plan("x", Path::new("/unused"), WorkflowStep::CodeReview)],
        ),
        Err(WorkflowError::Project(ProjectError::TaskBusy))
    ));
    fixture
        .manager
        .set_task_status(project_id, task_id, TaskStatus::Error)
        .unwrap();
    assert!(matches!(
        fixture.manager.retry_step(project_id, task_id),
        Err(ProjectError::TaskBusy)
    ));
    assert!(matches!(
        fixture.manager.retry_task(project_id, task_id),
        Err(ProjectError::TaskBusy)
    ));
    drop(lock);

    // Once the step process is gone, its unfinished agents are stopped and their panes closed.
    assert_eq!(
        cancel_interrupted_agents(&fixture.manager, &fixture.tools, project_id, task_id).unwrap(),
        1
    );
    assert_eq!(
        calls_starting_with(&fixture.log, &["agent", "send-keys"]).len(),
        1
    );
    assert_eq!(
        calls_starting_with(&fixture.log, &["pane", "close", "w1:stale"]).len(),
        1
    );
    let retried = fixture.manager.retry_step(project_id, task_id).unwrap();
    assert_eq!(retried.status, TaskStatus::InProgress);
    assert_eq!(retried.attempt, 1);
}
