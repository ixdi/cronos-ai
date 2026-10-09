use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

const EMPTY_GLOBAL_GIT_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

use cronos_ai::{
    integrations::GitAdapter,
    model::{ApprovalDecision, ApprovalGate, Priority, TaskStatus, WorkflowStep},
    project::ProjectManager,
    workflow::{AgentAdapter, AgentPlan, WorkflowEngine, WorkflowError},
};

struct Active<'a>(&'a AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct FakeAgents {
    first_codegen_barrier: Barrier,
    cancel_first_sibling: AtomicBool,
    fail_first_codegen: AtomicBool,
    active: AtomicUsize,
    maximum_active: Arc<AtomicUsize>,
}

impl FakeAgents {
    fn new() -> Self {
        Self {
            first_codegen_barrier: Barrier::new(2),
            cancel_first_sibling: AtomicBool::new(false),
            fail_first_codegen: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            maximum_active: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn observe_parallelism(&self) -> Active<'_> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        Active(&self.active)
    }
}

impl AgentAdapter for FakeAgents {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(
        &self,
        task: &cronos_ai::model::Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<String, String> {
        let _active = self.observe_parallelism();
        let worktree = PathBuf::from(
            task.worktree
                .as_deref()
                .ok_or_else(|| "missing worktree".to_owned())?,
        );

        if task.attempt == 1 && step == WorkflowStep::CodeGeneration {
            self.first_codegen_barrier.wait();
            if agent.name == "builder" && self.fail_first_codegen.swap(false, Ordering::SeqCst) {
                return Err("simulated implementation failure".to_owned());
            }
            while !self.cancel_first_sibling.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            return Ok("late success after cancellation".to_owned());
        }

        if step == WorkflowStep::SpecificationGeneration {
            thread::sleep(Duration::from_millis(20));
        }
        if step == WorkflowStep::CodeGeneration && agent.name == "builder" {
            let source = worktree.join("src/generated.txt");
            fs::create_dir_all(source.parent().unwrap()).map_err(|error| error.to_string())?;
            fs::write(source, "generated implementation\n").map_err(|error| error.to_string())?;
        }
        if step == WorkflowStep::Documentation {
            let documentation = worktree.join("docs/workflow.md");
            fs::create_dir_all(documentation.parent().unwrap())
                .map_err(|error| error.to_string())?;
            fs::write(documentation, "Generated and approved locally.\n")
                .map_err(|error| error.to_string())?;
            git(worktree.as_path(), &["add", "-A"])?;
            git(
                worktree.as_path(),
                &["commit", "-m", "complete workflow output"],
            )?;
        }
        Ok(format!("{} completed {}", agent.name, step.as_str()))
    }

    fn cancel(&self, _task: &cronos_ai::model::Task, step: WorkflowStep, _agent: &AgentPlan) {
        if step == WorkflowStep::CodeGeneration {
            self.cancel_first_sibling.store(true, Ordering::SeqCst);
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", EMPTY_GLOBAL_GIT_CONFIG)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn init_repository(root: &Path) {
    fs::create_dir_all(root).unwrap();
    git(root, &["init"]).unwrap();
    git(root, &["config", "user.name", "Cronos E2E"]).unwrap();
    git(
        root,
        &["config", "user.email", "cronos-e2e@example.invalid"],
    )
    .unwrap();
    fs::write(root.join("README.md"), "start\n").unwrap();
    git(root, &["add", "README.md"]).unwrap();
    git(root, &["commit", "-m", "initial"]).unwrap();
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
fn task_workflow_recovers_then_approves_documents_and_merges_locally() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let data_dir = temp.path().join("cronos-data");
    init_repository(&repository);

    let manager = ProjectManager::new(&data_dir).unwrap();
    let git = GitAdapter::default();
    let project = manager
        .create_project_with_repository("E2E".to_owned(), None, Some(&repository), &git)
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
    let agents = FakeAgents::new();
    let maximum_active = agents.maximum_active.clone();
    let engine = WorkflowEngine::new(ProjectManager::new(&data_dir).unwrap(), agents);
    engine.start_next_task(project.id).unwrap().unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git)
        .unwrap();

    engine
        .run_step(
            project.id,
            task.id,
            WorkflowStep::SpecificationGeneration,
            plans(&["spec_writer", "spec_reviewer"]),
        )
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
    assert!(matches!(
        engine.run_step(
            project.id,
            task.id,
            WorkflowStep::CodeGeneration,
            plans(&["builder", "reviewer"]),
        ),
        Err(WorkflowError::AgentFailed { .. })
    ));
    let failed = manager.get_task(project.id, task.id).unwrap();
    let failed_worktree = PathBuf::from(failed.worktree.as_deref().unwrap());
    assert_eq!(failed.status, TaskStatus::Error);
    assert!(failed.agent_activity.iter().any(|event| {
        event.attempt == 1 && event.status == cronos_ai::model::AgentStatus::Cancelled
    }));

    let retry = manager
        .retry_task_with_git(project.id, task.id, &git)
        .unwrap();
    assert_eq!(retry.attempt, 2);
    assert!(!failed_worktree.exists());
    manager
        .prepare_task_worktree(project.id, task.id, &git)
        .unwrap();

    engine
        .run_step(
            project.id,
            task.id,
            WorkflowStep::SpecificationGeneration,
            plans(&["spec_writer", "spec_reviewer"]),
        )
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
    for (step, agents) in [
        (
            WorkflowStep::CodeGeneration,
            plans(&["builder", "reviewer"]),
        ),
        (WorkflowStep::CodeReview, plans(&["reviewer", "security"])),
        (WorkflowStep::Verification, plans(&["tests", "quality"])),
    ] {
        engine.run_step(project.id, task.id, step, agents).unwrap();
    }
    manager
        .record_approval(
            project.id,
            task.id,
            ApprovalGate::FinalCode,
            ApprovalDecision::Approved,
            None,
        )
        .unwrap();
    engine
        .run_step(
            project.id,
            task.id,
            WorkflowStep::Documentation,
            plans(&["docs"]),
        )
        .unwrap();
    assert!(maximum_active.load(Ordering::SeqCst) >= 2);

    let completed = manager
        .merge_and_complete_task(project.id, task.id, &git)
        .unwrap();
    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(completed.attempt, 2);
    assert!(completed.is_approved(ApprovalGate::Specifications));
    assert!(completed.is_approved(ApprovalGate::FinalCode));
    assert_eq!(
        fs::read_to_string(repository.join("src/generated.txt")).unwrap(),
        "generated implementation\n"
    );
    assert_eq!(
        fs::read_to_string(repository.join("docs/workflow.md")).unwrap(),
        "Generated and approved locally.\n"
    );
}
