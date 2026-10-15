//! Opt-in smoke test against real Herdr and Pi. Run from a Herdr pane with a configured Pi model:
//! `CRONOS_LIVE_SMOKE=1 cargo test --test live_smoke -- --ignored --nocapture`
#![cfg(unix)]

use std::{fs, path::Path, process::Command};

use cronos_ai::{
    agent_profiles::AgentProfileCatalog,
    integrations::GitAdapter,
    model::{AgentStatus, ApprovalDecision, ApprovalGate, Priority, WorkflowStep},
    project::ProjectManager,
    step_skills::install_step_skills,
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
        HerdrAgentAdapter::with_profiles(AgentProfileCatalog::default())
            .with_step_skills(install_step_skills(&data_dir).unwrap()),
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

#[test]
#[ignore = "asks the real Pi step planner for a plan; set CRONOS_LIVE_SMOKE=1"]
fn real_pi_step_planner_proposes_a_valid_plan() {
    use cronos_ai::{
        integrations::ToolCommands,
        orchestration::{
            PiStepPlanner, StepContext, StepPlanner, parse_step_plan, step_planning_prompt,
        },
    };

    if std::env::var_os("CRONOS_LIVE_SMOKE").is_none_or(|value| value != "1") {
        eprintln!("skipped: requires CRONOS_LIVE_SMOKE=1");
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
        "# Calculator\n\nA CLI that adds two numbers.\n",
    )
    .unwrap();
    fs::write(
        repository.join("openspec/config.yaml"),
        "schema: spec-driven\n",
    )
    .unwrap();
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-m", "initial"]);
    let manager = ProjectManager::new(temp.path().join("data")).unwrap();
    let git_adapter = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "Calculator".to_owned(),
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
            "Add subtraction".to_owned(),
            Some("Users can subtract two numbers from the CLI.".to_owned()),
            Priority::High,
            Vec::new(),
        )
        .unwrap();
    manager.start_next_task(project.id).unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git_adapter)
        .unwrap();
    let task = manager.get_task(project.id, task.id).unwrap();
    let step = WorkflowStep::SpecificationGeneration;
    let profiles = AgentProfileCatalog::default();
    let prompt = step_planning_prompt(&task, step, &StepContext::for_task(&task, step), &profiles);
    let planner = PiStepPlanner {
        tools: ToolCommands::default(),
        model: None,
        timeout: std::time::Duration::from_secs(300),
    };

    let answer = planner.plan(&task, step, &prompt).unwrap();
    let plan = parse_step_plan(&answer, step, &profiles)
        .unwrap_or_else(|error| panic!("{error}\n--- answer ---\n{answer}"));

    eprintln!("planned: {plan:#?}");
    assert_eq!(plan.len(), 1);
    assert!(plan[0].prompt.to_lowercase().contains("subtract"));
}
