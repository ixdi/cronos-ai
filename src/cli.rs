use std::{env, error::Error, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use uuid::Uuid;

use crate::{
    agent_profiles::AgentProfileCatalog,
    integrations::{GitAdapter, ToolCommands, herdr_session_available, require_tools},
    model::{
        Prioritizer, Priority, ProjectId, PublishMode, RequirementId, RequirementKind, TaskId,
    },
    project::ProjectManager,
    workflow::{HerdrAgentAdapter, WorkflowEngine},
};

#[derive(Parser)]
#[command(name = "cronos-ai", about = "Supervised AI software factory")]
struct Cli {
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        action: InternalAction,
    },
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Manage the product requirements tasks are planned from.
    Requirement {
        #[command(subcommand)]
        action: RequirementAction,
    },
}

#[derive(Subcommand)]
enum RequirementAction {
    /// Add a requirement; its text is normalized and duplicates are rejected.
    Add {
        project_id: String,
        text: String,
        #[arg(long, value_enum, default_value_t = RequirementKindArg::Functional)]
        kind: RequirementKindArg,
    },
    List {
        project_id: String,
    },
    Remove {
        project_id: String,
        requirement_id: String,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RequirementKindArg {
    #[default]
    Functional,
    NonFunctional,
}

impl From<RequirementKindArg> for RequirementKind {
    fn from(value: RequirementKindArg) -> Self {
        match value {
            RequirementKindArg::Functional => Self::Functional,
            RequirementKindArg::NonFunctional => Self::NonFunctional,
        }
    }
}

#[derive(Subcommand)]
enum ProjectAction {
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_name = "PATH")]
        repository: Option<PathBuf>,
    },
    List,
    SetRepository {
        id: String,
        #[arg(long, value_name = "PATH")]
        repository: PathBuf,
    },
    /// Set the program and arguments run (without a shell) in the task worktree during verification.
    SetCheckCommand {
        id: String,
        #[arg(last = true, required = true, value_name = "PROGRAM [ARGS]...")]
        command: Vec<String>,
    },
    /// Set how many minutes (1-480) the check command may run; omit MINUTES to restore the 60-minute default.
    SetCheckTimeout {
        id: String,
        minutes: Option<u32>,
    },
    /// Set the model the task planner and the step planner run on; omit MODEL for Pi's default.
    SetPlannerModel {
        id: String,
        model: Option<String>,
    },
    /// Choose how the next task is picked: built-in priority order, or an agent prioritizer.
    SetPrioritizer {
        id: String,
        #[arg(value_enum)]
        prioritizer: PrioritizerArg,
        /// Model for the agent prioritizer (default: Pi's default model).
        #[arg(long)]
        model: Option<String>,
    },
    /// Run workers in a write sandbox that protects Cronos data, the base checkout, and Git hooks (on), or not (off).
    SetSandbox {
        id: String,
        #[arg(value_enum)]
        state: SwitchArg,
    },
    /// Set how many tasks may run at the same time (1-8).
    SetParallelism {
        id: String,
        max_parallel_tasks: u32,
    },
    /// Ask the read-only planner for tasks covering the requirements; the plan waits for approval.
    Plan {
        id: String,
        /// Minutes the planner may run.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=480))]
        timeout_minutes: u64,
    },
    /// Print the plan waiting for approval, or null.
    PlanShow {
        id: String,
    },
    /// Create the pending plan's tasks.
    PlanApply {
        id: String,
    },
    /// Throw the pending plan away.
    PlanDiscard {
        id: String,
    },
    /// Tag each locally merged task as the next release and add it to CHANGELOG.md (on), or not (off).
    SetReleases {
        id: String,
        #[arg(value_enum)]
        state: SwitchArg,
    },
    /// Set how completed tasks are published: merged locally, merged and pushed, or opened as a pull request.
    SetPublish {
        id: String,
        #[arg(value_enum)]
        mode: PublishArg,
        /// Git remote to push to (default: origin).
        #[arg(long)]
        remote: Option<String>,
    },
    Select {
        id: String,
    },
    Update {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    Archive {
        id: String,
    },
}

#[derive(Subcommand)]
enum InternalAction {
    Launch,
    Workflow {
        #[command(subcommand)]
        action: InternalWorkflowAction,
    },
}

#[derive(Subcommand)]
enum InternalWorkflowAction {
    /// Run the task's agent steps until a human approval, the local merge, Error, or Completed.
    RunTask { project_id: String, task_id: String },
}

#[derive(Subcommand)]
enum TaskAction {
    Create {
        project_id: String,
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_enum, default_value_t = PriorityArg::Medium)]
        priority: PriorityArg,
        #[arg(long = "depends-on", value_delimiter = ',')]
        dependencies: Vec<String>,
    },
    List {
        project_id: String,
    },
    Ready {
        project_id: String,
    },
    /// Show the order in which startable tasks would start, asking the agent prioritizer when the
    /// project uses it. Starts nothing.
    Rank {
        project_id: String,
        /// Seconds the agent prioritizer may take.
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=1800))]
        timeout_seconds: u64,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PrioritizerArg {
    BuiltIn,
    Agent,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SwitchArg {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PublishArg {
    Local,
    Push,
    PullRequest,
}

impl From<PublishArg> for PublishMode {
    fn from(value: PublishArg) -> Self {
        match value {
            PublishArg::Local => Self::Local,
            PublishArg::Push => Self::Push,
            PublishArg::PullRequest => Self::PullRequest,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum PriorityArg {
    Low,
    #[default]
    Medium,
    High,
}

impl From<PriorityArg> for Priority {
    fn from(value: PriorityArg) -> Self {
        match value {
            PriorityArg::Low => Self::Low,
            PriorityArg::Medium => Self::Medium,
            PriorityArg::High => Self::High,
        }
    }
}

/// Run CLI commands; return the data directory when the caller should start the TUI.
pub fn run() -> Result<Option<PathBuf>, Box<dyn Error>> {
    let cli = Cli::parse();
    let root = cli.data_dir.unwrap_or(default_data_dir()?);
    let Some(command) = cli.command else {
        require_available_tools(&ToolCommands::default())?;
        return Ok(Some(root));
    };
    let manager = ProjectManager::new(root.clone())?;

    match command {
        Command::Internal { action } => match action {
            InternalAction::Launch => {
                return launch_or_open_tui(
                    root,
                    herdr_session_available(),
                    &ToolCommands::default(),
                );
            }
            InternalAction::Workflow { action } => match action {
                InternalWorkflowAction::RunTask {
                    project_id,
                    task_id,
                } => {
                    let project_id = parse_project_id(&project_id)?;
                    let task_id = parse_task_id(&task_id)?;
                    let profiles = AgentProfileCatalog::load(&root)?;
                    let mut adapter = HerdrAgentAdapter::with_profiles(profiles.clone())
                        .with_step_skills(crate::step_skills::install_step_skills(&root)?);
                    let project = manager.get_project(project_id)?;
                    if project.sandbox {
                        // A sandboxed project never runs its workers unprotected.
                        crate::sandbox::sandbox_available()?;
                        let repository = project
                            .repository
                            .as_deref()
                            .ok_or("a sandboxed project needs a repository")?;
                        adapter = adapter.with_sandbox(crate::sandbox::SandboxPolicy::for_project(
                            &root,
                            std::path::Path::new(repository),
                            &GitAdapter::default(),
                        )?);
                    }
                    let planner = crate::orchestration::PiStepPlanner {
                        tools: ToolCommands::default(),
                        model: project.planner_model.clone(),
                        timeout: STEP_PLANNER_TIMEOUT,
                    };
                    let engine = WorkflowEngine::with_profiles(manager, adapter, profiles.clone());
                    // Progress goes to stdout line by line; the dashboard shows the latest line.
                    let task = crate::orchestration::run_task(
                        &engine,
                        &planner,
                        &profiles,
                        project_id,
                        task_id,
                        &mut |line: &str| {
                            println!("{line}");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        },
                    )?;
                    println!(
                        "stopped at {} ({:?})",
                        task.current_step.as_deref().unwrap_or("no step"),
                        task.status
                    );
                }
            },
        },
        Command::Project { action } => match action {
            ProjectAction::Create {
                name,
                description,
                repository,
            } => {
                print_json(&manager.create_project_with_repository(
                    name,
                    description,
                    repository.as_deref(),
                    None,
                    None,
                    &GitAdapter::default(),
                )?)?;
            }
            ProjectAction::List => print_json(&manager.list_projects()?)?,
            ProjectAction::SetRepository { id, repository } => {
                print_json(&manager.set_project_repository(
                    parse_project_id(&id)?,
                    &repository,
                    &GitAdapter::default(),
                )?)?;
            }
            ProjectAction::SetCheckCommand { id, command } => {
                print_json(&manager.set_project_check_command(parse_project_id(&id)?, command)?)?;
            }
            ProjectAction::SetPlannerModel { id, model } => {
                print_json(&manager.set_project_planner_model(parse_project_id(&id)?, model)?)?;
            }
            ProjectAction::SetPrioritizer {
                id,
                prioritizer,
                model,
            } => {
                let prioritizer = match prioritizer {
                    PrioritizerArg::BuiltIn => Prioritizer::BuiltIn,
                    PrioritizerArg::Agent => Prioritizer::Agent,
                };
                print_json(&manager.set_project_prioritizer(
                    parse_project_id(&id)?,
                    prioritizer,
                    model,
                )?)?;
            }
            ProjectAction::SetSandbox { id, state } => {
                print_json(&manager.set_project_sandbox(
                    parse_project_id(&id)?,
                    matches!(state, SwitchArg::On),
                )?)?;
            }
            ProjectAction::SetParallelism {
                id,
                max_parallel_tasks,
            } => {
                print_json(
                    &manager.set_project_parallelism(parse_project_id(&id)?, max_parallel_tasks)?,
                )?;
            }
            ProjectAction::Plan {
                id,
                timeout_minutes,
            } => {
                print_json(&manager.plan_tasks(
                    parse_project_id(&id)?,
                    &ToolCommands::default(),
                    std::time::Duration::from_secs(timeout_minutes * 60),
                )?)?;
            }
            ProjectAction::PlanShow { id } => {
                print_json(&manager.pending_plan(parse_project_id(&id)?)?)?;
            }
            ProjectAction::PlanApply { id } => {
                print_json(&manager.apply_plan(parse_project_id(&id)?)?)?;
            }
            ProjectAction::PlanDiscard { id } => {
                manager.discard_plan(parse_project_id(&id)?)?;
                print_json(&serde_json::Value::Null)?;
            }
            ProjectAction::SetReleases { id, state } => {
                print_json(&manager.set_project_releases(
                    parse_project_id(&id)?,
                    matches!(state, SwitchArg::On),
                )?)?;
            }
            ProjectAction::SetPublish { id, mode, remote } => {
                print_json(&manager.set_project_publish(
                    parse_project_id(&id)?,
                    mode.into(),
                    remote,
                )?)?;
            }
            ProjectAction::SetCheckTimeout { id, minutes } => {
                print_json(&manager.set_project_check_timeout(parse_project_id(&id)?, minutes)?)?;
            }
            ProjectAction::Select { id } => {
                print_json(&manager.select_project(parse_project_id(&id)?)?)?;
            }
            ProjectAction::Update {
                id,
                name,
                description,
            } => {
                if name.is_none() && description.is_none() {
                    return Err("project update requires --name or --description".into());
                }
                print_json(&manager.update_project(parse_project_id(&id)?, name, description)?)?;
            }
            ProjectAction::Archive { id } => {
                print_json(&manager.archive_project(parse_project_id(&id)?)?)?;
            }
        },
        Command::Task { action } => match action {
            TaskAction::Create {
                project_id,
                title,
                description,
                priority,
                dependencies,
            } => {
                let dependencies = dependencies
                    .iter()
                    .map(|id| parse_task_id(id))
                    .collect::<Result<Vec<_>, _>>()?;
                print_json(&manager.create_task(
                    parse_project_id(&project_id)?,
                    title,
                    description,
                    priority.into(),
                    dependencies,
                )?)?;
            }
            TaskAction::List { project_id } => {
                print_json(&manager.list_tasks(parse_project_id(&project_id)?)?)?;
            }
            TaskAction::Ready { project_id } => {
                print_json(&manager.ready_tasks(parse_project_id(&project_id)?)?)?;
            }
            TaskAction::Rank {
                project_id,
                timeout_seconds,
            } => {
                let project_id = parse_project_id(&project_id)?;
                let project = manager.get_project(project_id)?;
                let (startable, _) = manager.startable_tasks(project_id)?;
                let ranking = manager.rank_startable_tasks(
                    project_id,
                    &ToolCommands::default(),
                    std::time::Duration::from_secs(timeout_seconds),
                )?;
                let (order, reason) = match ranking {
                    Some(ranking) => (ranking.order, Some(ranking.reason)),
                    None => (startable.iter().map(|task| task.id).collect(), None),
                };
                let order = order
                    .iter()
                    .filter_map(|id| startable.iter().find(|task| task.id == *id))
                    .map(|task| serde_json::json!({"id": task.id, "title": task.title}))
                    .collect::<Vec<_>>();
                print_json(&serde_json::json!({
                    "prioritizer": project.prioritizer,
                    "order": order,
                    "reason": reason,
                }))?;
            }
        },
        Command::Requirement { action } => match action {
            RequirementAction::Add {
                project_id,
                text,
                kind,
            } => {
                print_json(&manager.add_requirement(
                    parse_project_id(&project_id)?,
                    kind.into(),
                    text,
                )?)?;
            }
            RequirementAction::List { project_id } => {
                print_json(
                    &manager
                        .get_project(parse_project_id(&project_id)?)?
                        .requirements,
                )?;
            }
            RequirementAction::Remove {
                project_id,
                requirement_id,
            } => {
                print_json(&manager.remove_requirement(
                    parse_project_id(&project_id)?,
                    RequirementId(Uuid::parse_str(&requirement_id)?),
                )?)?;
            }
        },
    }

    Ok(None)
}

fn default_data_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = env::var_os("CRONOS_AI_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(not(target_os = "windows"))]
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));

    base.map(|path| path.join("cronos-ai"))
        .ok_or_else(|| "cannot determine data directory; pass --data-dir".into())
}

const CRONOS_AI_HERDR_SESSION: &str = "cronos-ai";
/// How long the step planner may take before the default plan runs.
const STEP_PLANNER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

fn require_available_tools(tools: &ToolCommands) -> Result<(), Box<dyn Error>> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    require_tools(
        || tools.missing_required_tools(),
        &mut stdin.lock(),
        &mut stdout.lock(),
    )?;
    Ok(())
}

fn launch_or_open_tui(
    data_dir: PathBuf,
    inside_herdr: bool,
    tools: &ToolCommands,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if inside_herdr {
        require_available_tools(tools)?;
        return Ok(Some(data_dir));
    }

    let pane_id = tools.herdr_create_workspace(CRONOS_AI_HERDR_SESSION, &env::current_dir()?)?;
    tools.herdr_run_tui(CRONOS_AI_HERDR_SESSION, &pane_id, &env::current_exe()?)?;
    tools.herdr_attach_session(CRONOS_AI_HERDR_SESSION)?;
    Ok(None)
}

fn parse_project_id(value: &str) -> Result<ProjectId, uuid::Error> {
    Uuid::parse_str(value).map(ProjectId)
}

fn parse_task_id(value: &str) -> Result<TaskId, uuid::Error> {
    Uuid::parse_str(value).map(TaskId)
}

fn print_json(value: &impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, launch_or_open_tui};
    use crate::integrations::ToolCommands;
    use clap::Parser;
    use std::fs;
    #[cfg(unix)]
    use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

    #[cfg(unix)]
    fn fake_herdr(directory: &std::path::Path, response: &str) -> PathBuf {
        let path = directory.join("herdr");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$(dirname \"$0\")/args\"\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nif [ \"$3\" = \"workspace\" ] && [ \"$4\" = \"create\" ]; then printf '%s' '{response}'; fi\n"
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    fn launcher_tools(herdr: PathBuf) -> ToolCommands {
        ToolCommands::with_paths(herdr.clone(), herdr.clone(), herdr.clone(), herdr)
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cronos_ai_script_resolves_binary_from_its_location_not_the_callers_directory() {
        let root = tempfile::tempdir().unwrap();
        let scripts = root.path().join("scripts");
        let binary = root.path().join("target/release/cronos-ai");
        let caller = root.path().join("different-working-directory");
        let log = root.path().join("arguments");
        fs::create_dir_all(&scripts).unwrap();
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&caller).unwrap();
        fs::write(
            scripts.join("cronos-ai"),
            include_str!("../scripts/cronos-ai"),
        )
        .unwrap();
        make_executable(
            &binary,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$LOG_PATH\"\n",
        );

        let output = Command::new("bash")
            .arg(scripts.join("cronos-ai"))
            .current_dir(caller)
            .env("LOG_PATH", &log)
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(fs::read_to_string(log).unwrap(), "internal\nlaunch\n");
    }

    #[cfg(unix)]
    #[test]
    fn cronos_ai_script_reports_how_to_build_when_release_binary_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let scripts = root.path().join("scripts");
        let caller = root.path().join("different-working-directory");
        fs::create_dir_all(&scripts).unwrap();
        fs::create_dir_all(&caller).unwrap();
        fs::write(
            scripts.join("cronos-ai"),
            include_str!("../scripts/cronos-ai"),
        )
        .unwrap();

        let output = Command::new("bash")
            .arg(scripts.join("cronos-ai"))
            .current_dir(caller)
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cargo build --release"));
    }

    #[cfg(unix)]
    #[test]
    fn start_inside_herdr_runs_normal_preflight_without_session_operations() {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_herdr(
            temp.path(),
            r#"{"result":{"root_pane":{"pane_id":"w1:p1"}}}"#,
        );
        let data_dir = temp.path().join("data");

        let result = launch_or_open_tui(data_dir.clone(), true, &launcher_tools(fake)).unwrap();

        assert_eq!(result, Some(data_dir));
        let calls = fs::read_to_string(temp.path().join("args")).unwrap();
        assert!(!calls.contains("workspace"));
        assert!(!calls.contains("pane"));
        assert!(!calls.contains("--session"));
    }

    #[cfg(unix)]
    #[test]
    fn start_outside_herdr_creates_workspace_runs_tui_and_attaches() {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_herdr(
            temp.path(),
            r#"{"result":{"root_pane":{"pane_id":"w5:p8"}}}"#,
        );

        let result =
            launch_or_open_tui(temp.path().join("data"), false, &launcher_tools(fake)).unwrap();

        assert!(result.is_none());
        let calls = fs::read_to_string(temp.path().join("args")).unwrap();
        assert!(calls.contains("workspace\ncreate\n"));
        assert!(calls.contains("pane\nrun\nw5:p8\n"));
        assert!(calls.ends_with("--session\ncronos-ai\n"));
    }

    #[cfg(unix)]
    #[test]
    fn start_outside_herdr_stops_on_invalid_workspace_response() {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_herdr(temp.path(), r#"{"result":{"workspace":{}}}"#);

        assert!(
            launch_or_open_tui(temp.path().join("data"), false, &launcher_tools(fake),).is_err()
        );
        let calls = fs::read_to_string(temp.path().join("args")).unwrap();
        assert!(calls.contains("workspace\ncreate\n"));
        assert!(!calls.contains("pane\nrun"));
        assert!(!calls.ends_with("--session\ncronos-ai\n"));
    }

    #[test]
    fn internal_launcher_subcommand_is_accepted() {
        assert!(Cli::try_parse_from(["cronos-ai", "internal", "launch"]).is_ok());
    }

    #[test]
    fn project_and_task_commands_accept_expected_arguments() {
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "--data-dir",
                "/tmp/cronos",
                "project",
                "create",
                "Factory",
                "--description",
                "Local project",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "project",
                "create",
                "Factory",
                "--repository",
                "./repo",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "project",
                "set-repository",
                "00000000-0000-0000-0000-000000000000",
                "--repository",
                "./repo",
            ])
            .is_ok()
        );
        let Cli {
            command:
                Some(super::Command::Project {
                    action: super::ProjectAction::SetCheckCommand { command, .. },
                }),
            ..
        } = Cli::try_parse_from([
            "cronos-ai",
            "project",
            "set-check-command",
            "00000000-0000-0000-0000-000000000000",
            "--",
            "./scripts/check.sh",
            "--all",
        ])
        .unwrap()
        else {
            panic!("expected set-check-command");
        };
        assert_eq!(command, ["./scripts/check.sh", "--all"]);
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "project",
                "set-check-command",
                "00000000-0000-0000-0000-000000000000",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "task",
                "create",
                "00000000-0000-0000-0000-000000000000",
                "Implement storage",
                "--priority",
                "high",
                "--depends-on",
                "00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "internal",
                "workflow",
                "run-task",
                "00000000-0000-0000-0000-000000000000",
                "00000000-0000-0000-0000-000000000001",
            ])
            .is_ok()
        );
    }
}
