//! Opt-in smoke test against real Herdr and Pi. Run from a Herdr pane with a configured Pi model:
//! `CRONOS_LIVE_SMOKE=1 cargo test --test live_smoke -- --ignored --nocapture`
#![cfg(unix)]

use std::{fs, path::Path, process::Command};

use cronos_ai::{
    agent_profiles::AgentProfileCatalog,
    integrations::GitAdapter,
    model::{AgentStatus, ApprovalDecision, ApprovalGate, Priority, WorkflowStep},
    project::ProjectManager,
    workflow::{AgentPlan, HerdrAgentAdapter, WorkflowEngine},
};

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

#[test]
#[ignore = "starts a real Pi agent through Herdr; set CRONOS_LIVE_SMOKE=1 and run inside Herdr"]
fn real_herdr_pi_review_agent_writes_a_structured_result() {
    if std::env::var_os("CRONOS_LIVE_SMOKE").is_none_or(|value| value != "1")
        || std::env::var_os("HERDR_ENV").is_none_or(|value| value != "1")
    {
        eprintln!("skipped: requires CRONOS_LIVE_SMOKE=1 inside a Herdr pane");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("openspec")).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Cronos Smoke"]);
    git(
        &repository,
        &["config", "user.email", "cronos-smoke@example.invalid"],
    );
    fs::write(
        repository.join("README.md"),
        "# Smoke\n\nAdds two numbers.\n",
    )
    .unwrap();
    fs::write(
        repository.join("openspec/config.yaml"),
        "schema: spec-driven\n",
    )
    .unwrap();
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-m", "initial"]);

    let data_dir = temp.path().join("data");
    let manager = ProjectManager::new(&data_dir).unwrap();
    let git_adapter = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "Smoke".to_owned(),
            None,
            Some(&repository),
            Some(vec!["true".to_owned()]),
            None,
            &git_adapter,
        )
        .unwrap();
    let task = manager
        .create_task(
            project.id,
            "Review README".to_owned(),
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
    manager
        .complete_workflow_step(project.id, task.id, WorkflowStep::CodeGeneration)
        .unwrap();

    let engine = WorkflowEngine::with_profiles(
        ProjectManager::new(&data_dir).unwrap(),
        HerdrAgentAdapter::with_profiles(AgentProfileCatalog::default()),
        AgentProfileCatalog::default(),
    );
    let reviewed = engine
        .run_step(
            project.id,
            task.id,
            WorkflowStep::CodeReview,
            vec![AgentPlan {
                name: "reviewer".to_owned(),
                prompt: "Read README.md and confirm it describes the project. Report success."
                    .to_owned(),
                profile_id: None,
            }],
        )
        .unwrap();
    assert!(reviewed.agent_activity.iter().any(|event| {
        event.agent_name == "reviewer"
            && matches!(
                event.status,
                AgentStatus::Completed | AgentStatus::ChangesRequested
            )
    }));
}
