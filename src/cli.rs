use std::{env, error::Error, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use uuid::Uuid;

use crate::{
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
    },
    List,
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
    Task {
        #[command(subcommand)]
        action: InternalTaskAction,
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
        let tools = crate::integrations::ToolCommands::default();
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        crate::integrations::require_tools(
            || tools.missing_required_tools(),
            &mut stdin.lock(),
            &mut stdout.lock(),
        )?;
        return Ok(Some(root));
    };
    let manager = ProjectManager::new(root)?;

    match command {
        Command::Internal { action } => match action {
            InternalAction::Task { action } => match action {
                InternalTaskAction::Get {
                    project_id,
                    task_id,
                } => print_json(
                    &manager.get_task(parse_project_id(&project_id)?, parse_task_id(&task_id)?)?,
                )?,
            },
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
                    let task = WorkflowEngine::new(manager, HerdrAgentAdapter::default())
                        .run_step(
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
            ProjectAction::Create { name, description } => {
                print_json(&manager.create_project(name, description)?)?;
            }
            ProjectAction::List => print_json(&manager.list_projects()?)?,
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
    use super::Cli;
    use clap::Parser;

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
