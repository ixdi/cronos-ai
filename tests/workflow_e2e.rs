#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use cronos_ai::{
    agent_profiles::AgentProfileCatalog,
    integrations::{GitAdapter, GitError, ToolCommands},
    model::{
        AgentStatus, ApprovalDecision, ApprovalGate, Priority, Task, TaskStatus, WorkflowStep,
    },
    project::{MergeOutcome, ProjectError, ProjectManager},
    workflow::{AgentAdapter, AgentPlan, AgentReport, WorkflowEngine, WorkflowError},
};

const EMPTY_GLOBAL_GIT_CONFIG: &str = "/dev/null";

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", EMPTY_GLOBAL_GIT_CONFIG)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

/// Repository with a committed `openspec/` directory and a check command that requires "fixed".
fn init_repository(root: &Path) {
    fs::create_dir_all(root.join("openspec")).unwrap();
    git(root, &["init"]).unwrap();
    git(root, &["config", "user.name", "Cronos E2E"]).unwrap();
    git(
        root,
        &["config", "user.email", "cronos-e2e@example.invalid"],
    )
    .unwrap();
    fs::write(root.join("README.md"), "start\n").unwrap();
    fs::write(root.join("openspec/config.yaml"), "schema: spec-driven\n").unwrap();
    executable(
        &root.join("check.sh"),
        "#!/bin/sh\ngrep -q fixed src/generated.txt || { echo 'generated.txt is not fixed'; exit 1; }\n",
    );
    git(root, &["add", "-A"]).unwrap();
    git(root, &["commit", "-m", "initial"]).unwrap();
}

/// OpenSpec stand-in: validates that the change exists and archives it by moving it.
fn fake_openspec(dir: &Path) -> ToolCommands {
    let path = dir.join("openspec");
    executable(
        &path,
        "#!/bin/sh\ncase \"$1\" in\n  validate) test -d \"openspec/changes/$2\" ;;\n  archive) mkdir -p openspec/changes/archive && mv \"openspec/changes/$2\" \"openspec/changes/archive/$2\" ;;\nesac\n",
    );
    ToolCommands::with_paths("pi".into(), "herdr".into(), path, "git".into())
}

fn worktree(task: &Task) -> PathBuf {
    PathBuf::from(task.worktree.as_deref().unwrap())
}

fn commit(worktree: &Path, message: &str) -> Result<(), String> {
    git(worktree, &["add", "-A"])?;
    git(worktree, &["commit", "-m", message]).map(|_| ())
}

struct FakeAgents {
    fail_first_review: AtomicBool,
    security_waits_for_cancel: AtomicBool,
    review_requests_changes: AtomicBool,
    code_generations: AtomicUsize,
    review_barrier: Barrier,
    cancelled: Mutex<Vec<String>>,
    maximum_active: AtomicUsize,
    active: AtomicUsize,
}

impl AgentAdapter for FakeAgents {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String> {
        assert!(agent.prompt.contains("Cronos-AI contract"));
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        let result = self.work(task, step, agent);
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn cancel(&self, _task: &Task, _step: WorkflowStep, agent: &AgentPlan) {
        self.cancelled.lock().unwrap().push(agent.name.clone());
    }
}

impl FakeAgents {
    fn work(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String> {
        let worktree = worktree(task);
        match step {
            WorkflowStep::SpecificationGeneration => {
                let change = worktree
                    .join("openspec/changes")
                    .join(task.openspec_change());
                fs::create_dir_all(&change).map_err(|error| error.to_string())?;
                fs::write(change.join("proposal.md"), "# Proposal\n")
                    .map_err(|error| error.to_string())?;
                commit(&worktree, "docs(spec): specify the feature")?;
            }
            WorkflowStep::CodeGeneration => {
                let content = match self.code_generations.fetch_add(1, Ordering::SeqCst) {
                    0 => "draft\n",
                    1 => "fixed\n",
                    _ => "fixed\nreviewed\n",
                };
                fs::create_dir_all(worktree.join("src")).map_err(|error| error.to_string())?;
                fs::write(worktree.join("src/generated.txt"), content)
                    .map_err(|error| error.to_string())?;
                commit(&worktree, "feat: implement the feature")?;
            }
            WorkflowStep::CodeReview => {
                self.review_barrier.wait();
                if agent.name == "quality" && self.fail_first_review.swap(false, Ordering::SeqCst) {
                    return Err("simulated reviewer crash".to_owned());
                }
                if agent.name == "security"
                    && self.security_waits_for_cancel.swap(false, Ordering::SeqCst)
                {
                    while !self.cancelled.lock().unwrap().contains(&agent.name) {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    return Ok(AgentReport::success("late result is ignored"));
                }
                if agent.name == "quality"
                    && self.code_generations.load(Ordering::SeqCst) == 2
                    && self.review_requests_changes.swap(false, Ordering::SeqCst)
                {
                    return Ok(AgentReport::changes_requested(
                        "needs a review note",
                        vec!["add the reviewed marker".to_owned()],
                    ));
                }
            }
            WorkflowStep::Documentation => {
                let file = worktree.join(format!("docs/{}.md", agent.name));
                fs::create_dir_all(file.parent().unwrap()).map_err(|error| error.to_string())?;
                fs::write(file, "Generated and approved locally.\n")
                    .map_err(|error| error.to_string())?;
            }
            _ => {}
        }
        Ok(AgentReport::success(format!(
            "{} completed {}",
            agent.name,
            step.as_str()
        )))
    }
}

fn plans(names: &[&str]) -> Vec<AgentPlan> {
    names
        .iter()
        .map(|name| AgentPlan {
            name: (*name).to_owned(),
            prompt: format!("Run the {name} role for this test."),
            profile_id: None,
        })
        .collect()
}

#[test]
fn task_workflow_revises_retries_documents_archives_and_merges_locally() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let data_dir = temp.path().join("cronos-data");
    init_repository(&repository);

    let manager = ProjectManager::new(&data_dir).unwrap();
    let git_adapter = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "E2E".to_owned(),
            None,
            Some(&repository),
            Some(vec!["./check.sh".to_owned()]),
            None,
            &git_adapter,
        )
        .unwrap();
    let task = manager
        .create_task(
            project.id,
            "Build a small feature".to_owned(),
            None,
            Priority::High,
            Vec::new(),
        )
        .unwrap();
    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&data_dir).unwrap(),
        FakeAgents {
            fail_first_review: AtomicBool::new(true),
            security_waits_for_cancel: AtomicBool::new(true),
            review_requests_changes: AtomicBool::new(true),
            code_generations: AtomicUsize::new(0),
            review_barrier: Barrier::new(2),
            cancelled: Mutex::new(Vec::new()),
            maximum_active: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
        },
        AgentProfileCatalog::default(),
        fake_openspec(temp.path()),
    );
    engine.start_next_task(project.id).unwrap().unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git_adapter)
        .unwrap();
    let run = |step, agents: &[&str]| engine.run_step(project.id, task.id, step, plans(agents));

    assert!(matches!(
        run(WorkflowStep::SpecificationGeneration, &["writer", "second"]),
        Err(WorkflowError::InvalidAgentCount { .. })
    ));
    run(WorkflowStep::SpecificationGeneration, &["writer"]).unwrap();
    manager
        .record_approval(
            project.id,
            task.id,
            ApprovalGate::Specifications,
            ApprovalDecision::Approved,
            None,
        )
        .unwrap();
    run(WorkflowStep::CodeGeneration, &["builder"]).unwrap();

    // A crashing reviewer cancels its sibling; retrying the step keeps the attempt and approvals.
    assert!(matches!(
        run(WorkflowStep::CodeReview, &["quality", "security"]),
        Err(WorkflowError::AgentFailed { .. })
    ));
    let failed = manager.get_task(project.id, task.id).unwrap();
    assert_eq!(failed.status, TaskStatus::Error);
    assert!(
        failed
            .agent_activity
            .iter()
            .any(|event| event.status == AgentStatus::Cancelled)
    );
    let retried = manager.retry_step(project.id, task.id).unwrap();
    assert_eq!(retried.attempt, 1);
    assert_eq!(retried.current_step.as_deref(), Some("code-review"));
    assert!(retried.is_approved(ApprovalGate::Specifications));

    // Review passes; the check command fails on the draft and sends the task back.
    run(WorkflowStep::CodeReview, &["quality", "security"]).unwrap();
    let revised = run(WorkflowStep::Verification, &["tests", "security"]).unwrap();
    assert_eq!(revised.current_step.as_deref(), Some("code-generation"));
    assert_eq!(revised.automatic_revisions, 1);
    assert!(revised.agent_activity.iter().any(|event| {
        event.agent_name == "check-command"
            && event.status == AgentStatus::ChangesRequested
            && event.detail.as_deref().unwrap().contains("not fixed")
    }));

    // The reviewer requests changes once more, then everything passes.
    run(WorkflowStep::CodeGeneration, &["builder"]).unwrap();
    let revised = run(WorkflowStep::CodeReview, &["quality", "security"]).unwrap();
    assert_eq!(revised.current_step.as_deref(), Some("code-generation"));
    assert_eq!(revised.automatic_revisions, 2);
    run(WorkflowStep::CodeGeneration, &["builder"]).unwrap();
    run(WorkflowStep::CodeReview, &["quality", "security"]).unwrap();
    let verified = run(WorkflowStep::Verification, &["tests", "security"]).unwrap();
    assert_eq!(verified.current_step.as_deref(), Some("final-approval"));

    manager
        .record_approval(
            project.id,
            task.id,
            ApprovalGate::FinalCode,
            ApprovalDecision::Approved,
            None,
        )
        .unwrap();
    let documented = run(WorkflowStep::Documentation, &["guide", "reference"]).unwrap();
    assert_eq!(documented.current_step.as_deref(), Some("local-merge"));
    assert!(engine_parallelism_observed(&documented));

    let Ok(MergeOutcome::Completed(completed)) =
        manager.merge_and_complete_task(project.id, task.id, &git_adapter)
    else {
        panic!("expected a completed local merge");
    };
    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(
        fs::read_to_string(repository.join("src/generated.txt")).unwrap(),
        "fixed\nreviewed\n"
    );
    assert!(repository.join("docs/guide.md").is_file());
    assert!(repository.join("docs/reference.md").is_file());
    let change = task.openspec_change();
    assert!(
        repository
            .join("openspec/changes/archive")
            .join(&change)
            .is_dir()
    );
    assert!(!repository.join("openspec/changes").join(&change).exists());
    assert!(!repository.join(".cronos").exists());
    let log = git(&repository, &["log", "--format=%s"]).unwrap();
    assert!(log.contains("docs: document Build a small feature"));
}

fn engine_parallelism_observed(task: &Task) -> bool {
    // Both documentation agents recorded completions in the same step.
    task.agent_activity
        .iter()
        .filter(|event| event.step == "documentation" && event.status == AgentStatus::Completed)
        .count()
        == 2
}

struct MisbehavingAgents;

impl AgentAdapter for MisbehavingAgents {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String> {
        let worktree = worktree(task);
        // Reviewers edit files and writers leave uncommitted work: both must fail the step.
        fs::write(worktree.join(format!("{}.txt", agent.name)), "stray\n")
            .map_err(|error| error.to_string())?;
        Ok(AgentReport::success(format!(
            "{} {}",
            agent.name,
            step.as_str()
        )))
    }

    fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
}

#[test]
fn read_only_steps_and_uncommitted_writers_fail_the_task() {
    for step in [
        WorkflowStep::CodeGeneration,
        WorkflowStep::CodeReview,
        WorkflowStep::Verification,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let data_dir = temp.path().join("data");
        init_repository(&repository);
        let manager = ProjectManager::new(&data_dir).unwrap();
        let git_adapter = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "E2E".to_owned(),
                None,
                Some(&repository),
                Some(vec!["./check.sh".to_owned()]),
                None,
                &git_adapter,
            )
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Feature".to_owned(),
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
        let earlier: &[WorkflowStep] = match step {
            WorkflowStep::CodeReview => &[WorkflowStep::CodeGeneration],
            WorkflowStep::Verification => &[WorkflowStep::CodeGeneration, WorkflowStep::CodeReview],
            _ => &[],
        };
        for earlier in earlier {
            manager
                .complete_workflow_step(project.id, task.id, *earlier)
                .unwrap();
        }
        let engine = WorkflowEngine::with_tools(
            ProjectManager::new(&data_dir).unwrap(),
            MisbehavingAgents,
            AgentProfileCatalog::default(),
            fake_openspec(temp.path()),
        );
        assert!(matches!(
            engine.run_step(project.id, task.id, step, plans(&["agent"])),
            Err(WorkflowError::StepCheckFailed(_))
        ));
        let stored = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Error);
        assert_eq!(stored.current_step.as_deref(), Some(step.as_str()));
    }
}

/// A started task with a worktree, advanced to `target` without running agents.
fn task_at(root: &Path, target: WorkflowStep) -> (ProjectManager, PathBuf, PathBuf, Task) {
    task_with_check_at(root, Some(vec!["./check.sh".to_owned()]), target)
}

fn task_with_check_at(
    root: &Path,
    check_command: Option<Vec<String>>,
    target: WorkflowStep,
) -> (ProjectManager, PathBuf, PathBuf, Task) {
    let repository = root.join("repo");
    let data_dir = root.join("data");
    init_repository(&repository);
    let manager = ProjectManager::new(&data_dir).unwrap();
    let git_adapter = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "E2E".to_owned(),
            None,
            Some(&repository),
            check_command,
            None,
            &git_adapter,
        )
        .unwrap();
    let mut task = manager
        .create_task(
            project.id,
            "Feature".to_owned(),
            None,
            Priority::High,
            Vec::new(),
        )
        .unwrap();
    manager.start_next_task(project.id).unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git_adapter)
        .unwrap();
    loop {
        task = manager.get_task(project.id, task.id).unwrap();
        let current = WorkflowStep::parse(task.current_step.as_deref().unwrap()).unwrap();
        if current == target {
            return (manager, repository, data_dir, task);
        }
        let gate = match current {
            WorkflowStep::SpecificationApproval => Some(ApprovalGate::Specifications),
            WorkflowStep::FinalApproval => Some(ApprovalGate::FinalCode),
            _ => None,
        };
        match gate {
            Some(gate) => {
                manager
                    .record_approval(project.id, task.id, gate, ApprovalDecision::Approved, None)
                    .unwrap();
            }
            None => {
                manager
                    .complete_workflow_step(project.id, task.id, current)
                    .unwrap();
            }
        }
    }
}

/// Agents that commit a stray file in specification generation and otherwise do nothing.
struct PlainAgents;

impl AgentAdapter for PlainAgents {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String> {
        if step == WorkflowStep::SpecificationGeneration {
            let worktree = worktree(task);
            fs::write(worktree.join("notes.md"), "not an OpenSpec change\n")
                .map_err(|error| error.to_string())?;
            commit(&worktree, "docs: notes")?;
        }
        Ok(AgentReport::success(format!("{} done", agent.name)))
    }

    fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
}

#[test]
fn an_invalid_specification_marks_the_task_error() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, _, data_dir, task) = task_at(temp.path(), WorkflowStep::SpecificationGeneration);
    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&data_dir).unwrap(),
        PlainAgents,
        AgentProfileCatalog::default(),
        fake_openspec(temp.path()),
    );
    assert!(matches!(
        engine.run_step(
            task.project_id,
            task.id,
            WorkflowStep::SpecificationGeneration,
            plans(&["writer"])
        ),
        Err(WorkflowError::StepCheckFailed(message)) if message.contains("missing or invalid")
    ));
    let stored = manager.get_task(task.project_id, task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Error);
    assert_eq!(
        stored.current_step.as_deref(),
        Some("specification-generation")
    );
}

#[test]
fn a_check_that_leaves_files_or_was_changed_by_the_task_is_flagged() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, _, data_dir, task) = task_at(temp.path(), WorkflowStep::Verification);
    // The task branch rewrites the check program so it passes but leaves a build artifact.
    let worktree = worktree(&task);
    executable(
        &worktree.join("check.sh"),
        "#!/bin/sh\necho built > build.out\n",
    );
    commit(&worktree, "build: simplify the check").unwrap();

    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&data_dir).unwrap(),
        PlainAgents,
        AgentProfileCatalog::default(),
        fake_openspec(temp.path()),
    );
    let revised = engine
        .run_step(
            task.project_id,
            task.id,
            WorkflowStep::Verification,
            plans(&["tests"]),
        )
        .unwrap();
    assert_eq!(revised.current_step.as_deref(), Some("code-generation"));
    assert_eq!(revised.automatic_revisions, 1);
    let check = revised
        .agent_activity
        .iter()
        .rfind(|event| event.agent_name == "check-command")
        .unwrap();
    assert_eq!(check.status, AgentStatus::ChangesRequested);
    let detail = check.detail.as_deref().unwrap();
    assert!(
        detail.contains("WARNING") && detail.contains("check.sh"),
        "{detail}"
    );
    assert!(detail.contains("build.out"), "{detail}");
    assert_eq!(
        manager.get_task(task.project_id, task.id).unwrap().status,
        TaskStatus::InProgress
    );
}

#[test]
fn a_merge_failure_that_is_not_a_conflict_marks_the_task_error() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, repository, _, task) = task_at(temp.path(), WorkflowStep::LocalMerge);
    let worktree = worktree(&task);
    fs::write(worktree.join("feature.txt"), "done\n").unwrap();
    commit(&worktree, "feat: add feature").unwrap();
    // A failing hook stops the merge commit without leaving unmerged paths.
    let hooks = repository.join(".git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    executable(&hooks.join("pre-merge-commit"), "#!/bin/sh\nexit 1\n");

    let result = manager.merge_and_complete_task(task.project_id, task.id, &GitAdapter::default());
    assert!(matches!(
        result,
        Err(ProjectError::Git(GitError::MergeFailed(_)))
    ));
    let stored = manager.get_task(task.project_id, task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Error);
    assert_eq!(stored.automatic_revisions, 0);
    assert!(stored.agent_activity.iter().any(|event| {
        event.status == AgentStatus::Failed
            && event.detail.as_deref().unwrap().contains("needs a human")
    }));
    // The merge was aborted and the base is untouched.
    assert!(
        git(&repository, &["status", "--porcelain"])
            .unwrap()
            .trim()
            .is_empty()
    );
    assert!(!repository.join("feature.txt").exists());
}

#[test]
fn verification_without_a_check_command_marks_the_task_error() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, _, data_dir, task) =
        task_with_check_at(temp.path(), None, WorkflowStep::Verification);
    let engine = WorkflowEngine::with_tools(
        ProjectManager::new(&data_dir).unwrap(),
        PlainAgents,
        AgentProfileCatalog::default(),
        fake_openspec(temp.path()),
    );
    assert!(matches!(
        engine.run_step(
            task.project_id,
            task.id,
            WorkflowStep::Verification,
            plans(&["tests"])
        ),
        Err(WorkflowError::StepCheckFailed(message)) if message.contains("no check command")
    ));
    assert_eq!(
        manager.get_task(task.project_id, task.id).unwrap().status,
        TaskStatus::Error
    );
}
