use std::{env, error::Error, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use uuid::Uuid;

use crate::{
    agent_profiles::AgentProfileCatalog,
    integrations::{GitAdapter, ToolCommands, herdr_session_available, require_tools},
    model::{Priority, ProjectId, TaskId, WorkflowStep},
    project::ProjectManager,
    workflow::{AgentPlan, HerdrAgentAdapter, WorkflowEngine},
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
    Task {
        #[command(subcommand)]
        action: InternalTaskAction,
    },
    AgentProfiles {
        step: String,
    },
    Workflow {
        #[command(subcommand)]
        action: InternalWorkflowAction,
    },
}

#[derive(Subcommand)]
enum InternalTaskAction {
    Get { project_id: String, task_id: String },
}

#[derive(Subcommand)]
enum InternalWorkflowAction {
    RunStep {
        project_id: String,
        task_id: String,
        step: String,
        agents_json: String,
    },
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
            InternalAction::Task { action } => match action {
                InternalTaskAction::Get {
                    project_id,
                    task_id,
                } => print_json(
                    &manager.get_task(parse_project_id(&project_id)?, parse_task_id(&task_id)?)?,
                )?,
            },
            InternalAction::AgentProfiles { step } => {
                print_json(&list_agent_profiles(&root, &step)?)?;
            }
            InternalAction::Workflow { action } => match action {
                InternalWorkflowAction::RunStep {
                    project_id,
                    task_id,
                    step,
                    agents_json,
                } => {
                    let step = WorkflowStep::parse(&step).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "unknown workflow step",
                        )
                    })?;
                    let agents: Vec<AgentPlan> = serde_json::from_str(&agents_json)?;
                    let profiles = AgentProfileCatalog::load(&root)?;
                    let adapter = HerdrAgentAdapter::with_profiles(profiles.clone());
                    let task = WorkflowEngine::with_profiles(manager, adapter, profiles).run_step(
                        parse_project_id(&project_id)?,
                        parse_task_id(&task_id)?,
                        step,
                        agents,
                    )?;
                    print_json(&task)?;
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
        },
    }

    Ok(None)
}

#[derive(serde::Serialize)]
struct AgentProfileSummary {
    id: String,
    description: String,
}

fn list_agent_profiles(
    data_dir: &std::path::Path,
    step: &str,
) -> Result<Vec<AgentProfileSummary>, Box<dyn Error>> {
    let step = WorkflowStep::parse(step)
        .filter(|step| step.runs_agents())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unknown or non-agent workflow step",
            )
        })?;
    let catalog = AgentProfileCatalog::load(data_dir)?;
    Ok(catalog
        .for_step(step)
        .map(|profile| AgentProfileSummary {
            id: profile.id.clone(),
            description: profile.description.clone(),
        })
        .collect())
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
    use super::{Cli, launch_or_open_tui, list_agent_profiles};
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
    fn internal_agent_profiles_command_accepts_a_workflow_step() {
        assert!(
            Cli::try_parse_from(["cronos-ai", "internal", "agent-profiles", "code-review"]).is_ok()
        );
    }

    #[test]
    fn profile_listing_filters_by_agent_step_and_handles_empty_catalog() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("agent-profiles.json"),
            r#"{"version":1,"profiles":[{"id":"reviewer","description":"Reviews changes","steps":["code-review"]},{"id":"writer","description":"Writes docs","steps":["documentation"]}]}"#,
        )
        .unwrap();

        let profiles = list_agent_profiles(root.path(), "code-review").unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "reviewer");
        assert_eq!(profiles[0].description, "Reviews changes");

        assert!(
            list_agent_profiles(root.path(), "verification")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn profile_listing_rejects_unknown_and_non_agent_steps() {
        let root = tempfile::tempdir().unwrap();
        assert!(list_agent_profiles(root.path(), "not-a-step").is_err());
        assert!(list_agent_profiles(root.path(), "local-merge").is_err());
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
                "task",
                "get",
                "00000000-0000-0000-0000-000000000000",
                "00000000-0000-0000-0000-000000000001",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cronos-ai",
                "internal",
                "workflow",
                "run-step",
                "00000000-0000-0000-0000-000000000000",
                "00000000-0000-0000-0000-000000000001",
                "specification-generation",
                r#"[{"name":"spec-writer","prompt":"draft specs"}]"#,
            ])
            .is_ok()
        );
    }
}
