#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use cronos_ai::{
    agent_profiles::AgentProfileCatalog,
    integrations::{GitAdapter, ToolCommands},
    model::{ApprovalDecision, ApprovalGate, Priority, ProjectId, TaskId, WorkflowStep},
    project::ProjectManager,
    workflow::{AgentPlan, HerdrAgentAdapter, WorkflowEngine},
};

const EMPTY_GLOBAL_GIT_CONFIG: &str = "/dev/null";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", EMPTY_GLOBAL_GIT_CONFIG)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repository(root: &Path) {
    fs::create_dir_all(root).unwrap();
    git(root, &["init"]);
    git(root, &["config", "user.name", "Cronos E2E"]);
    git(
        root,
        &["config", "user.email", "cronos-e2e@example.invalid"],
    );
    fs::write(root.join("README.md"), "profile test\n").unwrap();
    fs::create_dir_all(root.join("openspec")).unwrap();
    fs::write(root.join("openspec/config.yaml"), "schema: spec-driven\n").unwrap();
    git(root, &["add", "README.md", "openspec"]);
    git(root, &["commit", "-m", "initial"]);
}

fn fake_herdr(path: &Path, log: &Path) {
    let log = format!("'{}'", log.to_string_lossy().replace('\'', "'\\''"));
    let script = format!(
        r##"#!/bin/sh
HERDR_LOG={log}
CALL_LOG="${{HERDR_LOG}}.$$"
printf '%s\0' "$@" > "$CALL_LOG"
if [ "$1" = "pane" ] && [ "$2" = "split" ]; then
  printf '{{"result":{{"pane":{{"pane_id":"w1:p3"}}}}}}'
elif [ "$1" = "agent" ] && [ "$2" = "prompt" ]; then
  result=$(printf '%s\n' "$4" | sed -n 's/.*write your result as JSON to `\([^`]*\)`.*/\1/p')
  mkdir -p "$(dirname "$result")"
  printf '{{"status":"success","summary":"completed by fake agent"}}' > "$result"
elif [ "$1" = "agent" ] && [ "$2" = "get" ]; then
  printf '{{"status":"done"}}'
elif [ "$1" = "agent" ] && [ "$2" = "read" ]; then
  printf 'completed by fake agent\n'
fi
"##
    );
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

fn commands(herdr: &Path) -> ToolCommands {
    ToolCommands::with_paths(
        herdr.to_path_buf(),
        herdr.to_path_buf(),
        herdr.to_path_buf(),
        herdr.to_path_buf(),
    )
}

fn load_and_run_step(
    data_dir: &Path,
    tools: ToolCommands,
    project_id: ProjectId,
    task_id: TaskId,
    agents: Vec<AgentPlan>,
) -> Result<(), Box<dyn std::error::Error>> {
    let profiles = AgentProfileCatalog::load(data_dir)?;
    let adapter = HerdrAgentAdapter::with_tools_and_profiles(tools, profiles.clone());
    let engine = WorkflowEngine::with_profiles(ProjectManager::new(data_dir)?, adapter, profiles);
    engine.run_step(project_id, task_id, WorkflowStep::CodeReview, agents)?;
    Ok(())
}

fn logged_calls(path: &Path) -> Vec<Vec<String>> {
    let prefix = format!("{}.", path.file_name().unwrap().to_string_lossy());
    let mut calls = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| fs::read(entry.path()).unwrap())
        .map(|bytes| {
            bytes
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
                .collect()
        })
        .collect::<Vec<Vec<String>>>();
    calls.sort();
    calls
}

fn contains_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

#[test]
fn workflow_launches_each_profile_with_only_its_resources_and_rejects_bad_mcp_before_start() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let data_dir = temp.path().join("cronos-data");
    let profiles_dir = temp.path().join("profiles");
    fs::create_dir_all(&profiles_dir).unwrap();
    init_repository(&repository);

    let writer_skill = profiles_dir.join("writer skill/SKILL.md");
    let writer_extension = profiles_dir.join("writer extension.ts");
    let reviewer_skill = profiles_dir.join("reviewer skill/SKILL.md");
    let reviewer_extension = profiles_dir.join("reviewer extension.ts");
    let mcp_adapter = profiles_dir.join("pi-mcp-adapter");
    let mcp_config = profiles_dir.join("reviewer-mcp.json");
    for path in [
        &writer_skill,
        &writer_extension,
        &reviewer_skill,
        &reviewer_extension,
        &mcp_adapter,
    ] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "resource placeholder\n").unwrap();
    }
    fs::write(
        &mcp_config,
        r#"{"mcpServers":{"docs":{"url":"https://example.invalid/mcp"}}}"#,
    )
    .unwrap();

    fs::create_dir_all(&data_dir).unwrap();
    let profile_file = serde_json::json!({
        "version": 1,
        "profiles": [
            {
                "id": "spec-writer",
                "description": "Reviews code quality",
                "steps": ["code-review"],
                "skills": [writer_skill],
                "extensions": [writer_extension]
            },
            {
                "id": "spec-reviewer",
                "description": "Reviews code with documentation search",
                "steps": ["code-review"],
                "skills": [reviewer_skill],
                "extensions": [reviewer_extension],
                "mcp": { "adapter": mcp_adapter, "config": mcp_config },
                "model": "provider/review-model",
                "timeout_minutes": 15
            }
        ]
    });
    fs::write(
        data_dir.join("agent-profiles.json"),
        serde_json::to_vec_pretty(&profile_file).unwrap(),
    )
    .unwrap();

    let manager = ProjectManager::new(&data_dir).unwrap();
    let git = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "Profile E2E".to_owned(),
            None,
            Some(&repository),
            Some(vec!["true".to_owned()]),
            None,
            &git,
        )
        .unwrap();
    let task = manager
        .create_task(
            project.id,
            "Exercise per-agent resources".to_owned(),
            None,
            Priority::High,
            Vec::new(),
        )
        .unwrap();
    manager.start_next_task(project.id).unwrap();
    manager
        .prepare_task_worktree(project.id, task.id, &git)
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

    let herdr = temp.path().join("fake-herdr");
    let log = temp.path().join("herdr-args.log");
    fake_herdr(&herdr, &log);
    load_and_run_step(
        &data_dir,
        commands(&herdr),
        project.id,
        task.id,
        vec![
            AgentPlan {
                name: "spec_writer".to_owned(),
                prompt: "Draft requirements".to_owned(),
                profile_id: Some("spec-writer".to_owned()),
            },
            AgentPlan {
                name: "spec_reviewer".to_owned(),
                prompt: "Review requirements".to_owned(),
                profile_id: Some("spec-reviewer".to_owned()),
            },
        ],
    )
    .unwrap();

    let starts = logged_calls(&log)
        .into_iter()
        .filter(|args| args.starts_with(&["agent".to_owned(), "start".to_owned()]))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2);
    let writer = starts
        .iter()
        .find(|args| contains_pair(args, "--skill", &writer_skill.to_string_lossy()))
        .unwrap();
    let reviewer = starts
        .iter()
        .find(|args| contains_pair(args, "--skill", &reviewer_skill.to_string_lossy()))
        .unwrap();
    for args in [&writer, &reviewer] {
        let delimiter = args.iter().position(|arg| arg == "--").unwrap();
        assert!(args[delimiter + 1..].contains(&"--no-skills".to_owned()));
        assert!(args[delimiter + 1..].contains(&"--no-extensions".to_owned()));
        assert!(args[delimiter + 1..].contains(&"--no-context-files".to_owned()));
        assert!(args[delimiter + 1..].contains(&"--no-mcp".to_owned()));
    }
    assert!(contains_pair(
        writer,
        "--skill",
        &writer_skill.to_string_lossy()
    ));
    assert!(contains_pair(
        writer,
        "--extension",
        &writer_extension.to_string_lossy()
    ));
    assert!(!writer.contains(&"--mcp-config".to_owned()));
    assert!(!writer.contains(&mcp_adapter.to_string_lossy().into_owned()));
    assert!(contains_pair(
        reviewer,
        "--skill",
        &reviewer_skill.to_string_lossy()
    ));
    assert!(contains_pair(
        reviewer,
        "--extension",
        &reviewer_extension.to_string_lossy()
    ));
    assert!(contains_pair(
        reviewer,
        "--extension",
        &mcp_adapter.to_string_lossy()
    ));
    assert!(contains_pair(
        reviewer,
        "--mcp-config",
        &mcp_config.to_string_lossy()
    ));
    assert!(!reviewer.contains(&writer_skill.to_string_lossy().into_owned()));
    assert!(!reviewer.contains(&writer_extension.to_string_lossy().into_owned()));
    assert!(contains_pair(reviewer, "--model", "provider/review-model"));
    assert!(!writer.contains(&"--model".to_owned()));

    let prompts = logged_calls(&log)
        .into_iter()
        .filter(|args| args.starts_with(&["agent".to_owned(), "prompt".to_owned()]))
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts
            .iter()
            .all(|args| args[3].contains("Cronos-AI contract"))
    );
    // Default code-review limit is 30 minutes; the reviewer profile sets 15.
    assert!(
        prompts
            .iter()
            .any(|args| contains_pair(args, "--timeout", "1800000"))
    );
    assert!(
        prompts
            .iter()
            .any(|args| contains_pair(args, "--timeout", "900000"))
    );
    let reviewed = manager.get_task(project.id, task.id).unwrap();
    assert_eq!(reviewed.current_step.as_deref(), Some("verification"));
    let closed = logged_calls(&log)
        .into_iter()
        .filter(|args| args.starts_with(&["pane".to_owned(), "close".to_owned()]))
        .count();
    assert_eq!(closed, 2);

    let invalid_dir = temp.path().join("invalid-profile-data");
    fs::create_dir_all(&invalid_dir).unwrap();
    let invalid_catalog = serde_json::json!({
        "version": 1,
        "profiles": [{
            "id": "broken-mcp",
            "description": "Invalid MCP adapter path",
            "steps": ["specification-generation"],
            "mcp": {
                "adapter": invalid_dir.join("missing-adapter.ts"),
                "config": mcp_config
            }
        }]
    });
    fs::write(
        invalid_dir.join("agent-profiles.json"),
        serde_json::to_vec(&invalid_catalog).unwrap(),
    )
    .unwrap();
    let calls_before = logged_calls(&log).len();
    assert!(
        load_and_run_step(
            &invalid_dir,
            commands(&herdr),
            ProjectId::new(),
            TaskId::new(),
            vec![AgentPlan {
                name: "reviewer".to_owned(),
                prompt: "Review".to_owned(),
                profile_id: Some("broken-mcp".to_owned()),
            }],
        )
        .is_err()
    );
    assert_eq!(logged_calls(&log).len(), calls_before);
}

#[test]
fn run_step_from_the_cli_gives_every_worker_the_built_in_skill_for_its_step() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let data_dir = temp.path().join("cronos-data");
    init_repository(&repository);

    let manager = ProjectManager::new(&data_dir).unwrap();
    let git = GitAdapter::default();
    let project = manager
        .create_project_with_repository(
            "Step skills".to_owned(),
            None,
            Some(&repository),
            Some(vec!["true".to_owned()]),
            None,
            &git,
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
        .prepare_task_worktree(project.id, task.id, &git)
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

    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = temp.path().join("herdr-args.log");
    fake_herdr(&bin.join("herdr"), &log);
    let output = Command::new(env!("CARGO_BIN_EXE_cronos-ai"))
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "internal",
            "workflow",
            "run-step",
            &project.id.0.to_string(),
            &task.id.0.to_string(),
            "code-review",
            r#"[{"name":"reviewer","prompt":"Review the branch"}]"#,
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("GIT_CONFIG_GLOBAL", EMPTY_GLOBAL_GIT_CONFIG)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let skill = data_dir.join("skills/cronos-code-review/SKILL.md");
    assert!(
        fs::read_to_string(&skill)
            .unwrap()
            .contains("name: cronos-code-review")
    );
    let starts = logged_calls(&log)
        .into_iter()
        .filter(|args| args.starts_with(&["agent".to_owned(), "start".to_owned()]))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1);
    assert!(contains_pair(
        &starts[0],
        "--skill",
        &skill.to_string_lossy()
    ));
}
