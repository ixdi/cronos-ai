use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    hash::{DefaultHasher, Hash, Hasher},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Mutex, mpsc},
    thread,
};

use serde::Deserialize;
use thiserror::Error;

use crate::{
    agent_profiles::{AgentProfile, AgentProfileCatalog},
    integrations::ToolCommands,
    model::{AgentStatus, ProjectId, Task, TaskId, TaskStatus, WorkflowStep},
    project::{ProjectError, ProjectManager},
};

const MAX_STEP_AGENTS: usize = 4;
const MAX_AGENT_PROMPT_CHARS: usize = 20_000;
const MAX_AGENT_SUMMARY_CHARS: usize = 2_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentPlan {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub profile_id: Option<String>,
}

#[derive(Default)]
pub struct HerdrAgentAdapter {
    tools: ToolCommands,
    pane_creation: Mutex<()>,
    profiles: AgentProfileCatalog,
}

impl HerdrAgentAdapter {
    pub fn with_profiles(profiles: AgentProfileCatalog) -> Self {
        Self::with_tools_and_profiles(ToolCommands::default(), profiles)
    }

    pub fn with_tools_and_profiles(tools: ToolCommands, profiles: AgentProfileCatalog) -> Self {
        Self {
            tools,
            pane_creation: Mutex::new(()),
            profiles,
        }
    }
}

impl AgentAdapter for HerdrAgentAdapter {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(&self, _task: &Task, _step: WorkflowStep, agent: &AgentPlan) -> Result<String, String> {
        let worktree = _task
            .worktree
            .as_deref()
            .ok_or_else(|| "task worktree is not set".to_owned())?;
        let worktree = Path::new(worktree);
        if !worktree.is_absolute() || !worktree.is_dir() {
            return Err("task worktree is unavailable".to_owned());
        }
        let herdr_name = herdr_agent_name(_task, _step, agent);
        let profile = agent
            .profile_id
            .as_deref()
            .and_then(|id| self.profiles.get_for_step(id, _step));
        let pi_args = pi_agent_args(profile);
        {
            let _guard = self
                .pane_creation
                .lock()
                .map_err(|_| "Herdr pane allocator is unavailable".to_owned())?;
            let pane_id = self
                .tools
                .herdr_split_pane(worktree)
                .map_err(|error| error.to_string())?;
            if let Err(error) = self
                .tools
                .herdr_start_pi_agent(&herdr_name, &pane_id, &pi_args)
            {
                self.cancel(_task, _step, agent);
                return Err(error.to_string());
            }
        }
        if let Err(error) = self.tools.herdr_prompt_agent(&herdr_name, &agent.prompt) {
            self.cancel(_task, _step, agent);
            return Err(error.to_string());
        }
        let status = match self.tools.herdr_agent_status(&herdr_name) {
            Ok(status) => status,
            Err(error) => {
                self.cancel(_task, _step, agent);
                return Err(error.to_string());
            }
        };
        match status.as_str() {
            "idle" | "done" => {}
            "blocked" => {
                self.cancel(_task, _step, agent);
                return Err("agent is blocked on an interactive prompt".to_owned());
            }
            status => {
                self.cancel(_task, _step, agent);
                return Err(format!("agent did not settle; status is {status}"));
            }
        }
        match self.tools.herdr_read_agent(&herdr_name, 120) {
            Ok(output) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
            Err(error) => {
                self.cancel(_task, _step, agent);
                Err(error.to_string())
            }
        }
    }

    fn cancel(&self, task: &Task, step: WorkflowStep, agent: &AgentPlan) {
        let _ = self
            .tools
            .herdr_cancel_agent(&herdr_agent_name(task, step, agent));
    }
}

pub trait AgentAdapter: Send + Sync {
    fn requires_worktree(&self) -> bool {
        false
    }

    fn run(&self, task: &Task, step: WorkflowStep, agent: &AgentPlan) -> Result<String, String>;

    fn cancel(&self, task: &Task, step: WorkflowStep, agent: &AgentPlan);
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error("step {0:?} is not an agent step")]
    NotAgentStep(WorkflowStep),
    #[error("an agent step requires 1 to {MAX_STEP_AGENTS} agents")]
    InvalidAgentCount,
    #[error("agent name must match [a-z][a-z0-9_-]{{0,31}}")]
    InvalidAgentName,
    #[error("agent names must be unique within a step: {0}")]
    DuplicateAgentName(String),
    #[error("agent prompt must be nonblank and at most {MAX_AGENT_PROMPT_CHARS} characters")]
    InvalidAgentPrompt,
    #[error("agent {agent} selected unavailable profile {profile:?} for workflow step {step:?}")]
    InvalidAgentProfile {
        agent: String,
        profile: String,
        step: WorkflowStep,
    },
    #[error("agent {name} failed: {message}")]
    AgentFailed { name: String, message: String },
}

pub struct WorkflowEngine<A> {
    manager: ProjectManager,
    adapter: A,
    profiles: AgentProfileCatalog,
}

impl<A: AgentAdapter> WorkflowEngine<A> {
    pub fn new(manager: ProjectManager, adapter: A) -> Self {
        Self {
            manager,
            adapter,
            profiles: AgentProfileCatalog::default(),
        }
    }

    pub fn with_profiles(
        manager: ProjectManager,
        adapter: A,
        profiles: AgentProfileCatalog,
    ) -> Self {
        Self {
            manager,
            adapter,
            profiles,
        }
    }

    pub fn start_next_task(&self, project_id: ProjectId) -> Result<Option<Task>, ProjectError> {
        self.manager.start_next_task(project_id)
    }

    pub fn run_step(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        step: WorkflowStep,
        agents: Vec<AgentPlan>,
    ) -> Result<Task, WorkflowError> {
        if !step.runs_agents() {
            return Err(WorkflowError::NotAgentStep(step));
        }
        validate_agents(&agents)?;
        validate_profile_assignments(&self.profiles, step, &agents)?;
        let mut task = self.manager.get_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status).into());
        }
        if task.current_step.as_deref() != Some(step.as_str()) {
            return Err(ProjectError::UnexpectedWorkflowStep {
                expected: step,
                actual: task.current_step.clone(),
            }
            .into());
        }
        if self.adapter.requires_worktree() {
            task.worktree = Some(
                self.manager
                    .validated_task_worktree_path(project_id, task_id)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        for agent in &agents {
            self.manager.record_agent_activity(
                project_id,
                task_id,
                agent.name.clone(),
                step.as_str().to_owned(),
                AgentStatus::Starting,
                None,
            )?;
        }

        match run_parallel(&self.adapter, &task, step, &agents) {
            Ok(results) => {
                for (agent, summary) in results {
                    self.manager.record_agent_activity(
                        project_id,
                        task_id,
                        agent,
                        step.as_str().to_owned(),
                        AgentStatus::Completed,
                        Some(truncate(&summary, MAX_AGENT_SUMMARY_CHARS)),
                    )?;
                }
                Ok(self
                    .manager
                    .complete_workflow_step(project_id, task_id, step)?)
            }
            Err(failure) => {
                for (agent, summary) in failure.completed {
                    self.manager.record_agent_activity(
                        project_id,
                        task_id,
                        agent,
                        step.as_str().to_owned(),
                        AgentStatus::Completed,
                        Some(truncate(&summary, MAX_AGENT_SUMMARY_CHARS)),
                    )?;
                }
                self.manager.record_agent_activity(
                    project_id,
                    task_id,
                    failure.name.clone(),
                    step.as_str().to_owned(),
                    AgentStatus::Failed,
                    Some(truncate(&failure.message, MAX_AGENT_SUMMARY_CHARS)),
                )?;
                for agent in failure.cancelled {
                    self.manager.record_agent_activity(
                        project_id,
                        task_id,
                        agent,
                        step.as_str().to_owned(),
                        AgentStatus::Cancelled,
                        None,
                    )?;
                }
                self.manager
                    .set_task_status(project_id, task_id, TaskStatus::Error)?;
                Err(WorkflowError::AgentFailed {
                    name: failure.name,
                    message: failure.message,
                })
            }
        }
    }
}

struct ParallelFailure {
    name: String,
    message: String,
    completed: Vec<(String, String)>,
    cancelled: Vec<String>,
}

fn run_parallel<A: AgentAdapter>(
    adapter: &A,
    task: &Task,
    step: WorkflowStep,
    agents: &[AgentPlan],
) -> Result<Vec<(String, String)>, ParallelFailure> {
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for agent in agents.iter().cloned() {
            let sender = sender.clone();
            let task = task.clone();
            scope.spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| adapter.run(&task, step, &agent)))
                    .unwrap_or_else(|_| Err("agent worker panicked".to_owned()));
                let _ = sender.send((agent.name, result));
            });
        }
        drop(sender);

        let mut pending = agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect::<HashSet<_>>();
        let mut completed = HashMap::new();
        let mut failure: Option<(String, String)> = None;
        let mut cancelled = HashSet::new();

        for _ in 0..agents.len() {
            let Ok((name, result)) = receiver.recv() else {
                break;
            };
            pending.remove(&name);
            match (failure.is_none(), result) {
                (true, Ok(summary)) => {
                    completed.insert(name, summary);
                }
                (true, Err(message)) => {
                    failure = Some((name, message));
                    for sibling in &pending {
                        if let Some(plan) = agents.iter().find(|agent| agent.name == *sibling) {
                            adapter.cancel(task, step, plan);
                        }
                        cancelled.insert(sibling.clone());
                    }
                }
                (false, _) => {
                    cancelled.insert(name);
                }
            }
        }

        if let Some((name, message)) = failure {
            Err(ParallelFailure {
                name,
                message,
                completed: completed.into_iter().collect(),
                cancelled: cancelled.into_iter().collect(),
            })
        } else if completed.len() == agents.len() {
            Ok(agents
                .iter()
                .filter_map(|agent| {
                    completed
                        .remove(&agent.name)
                        .map(|summary| (agent.name.clone(), summary))
                })
                .collect())
        } else {
            Err(ParallelFailure {
                name: "workflow".to_owned(),
                message: "an agent worker did not return a result".to_owned(),
                completed: completed.into_iter().collect(),
                cancelled: cancelled.into_iter().collect(),
            })
        }
    })
}

fn pi_agent_args(profile: Option<&AgentProfile>) -> Vec<OsString> {
    let mut args = [
        "--no-skills",
        "--no-extensions",
        "--no-context-files",
        "--no-mcp",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();

    if let Some(profile) = profile {
        for skill in &profile.skills {
            args.push("--skill".into());
            args.push(skill.as_os_str().to_owned());
        }
        for extension in &profile.extensions {
            args.push("--extension".into());
            args.push(extension.as_os_str().to_owned());
        }
        if let Some(mcp) = &profile.mcp {
            args.push("--extension".into());
            args.push(mcp.adapter.as_os_str().to_owned());
            args.push("--mcp-config".into());
            args.push(mcp.config.as_os_str().to_owned());
        }
    }
    args
}

fn validate_profile_assignments(
    profiles: &AgentProfileCatalog,
    step: WorkflowStep,
    agents: &[AgentPlan],
) -> Result<(), WorkflowError> {
    for agent in agents {
        if let Some(profile) = &agent.profile_id
            && profiles.get_for_step(profile, step).is_none()
        {
            return Err(WorkflowError::InvalidAgentProfile {
                agent: agent.name.clone(),
                profile: profile.clone(),
                step,
            });
        }
    }
    Ok(())
}

fn validate_agents(agents: &[AgentPlan]) -> Result<(), WorkflowError> {
    if agents.is_empty() || agents.len() > MAX_STEP_AGENTS {
        return Err(WorkflowError::InvalidAgentCount);
    }
    let mut names = HashSet::new();
    for agent in agents {
        if !valid_agent_name(&agent.name) {
            return Err(WorkflowError::InvalidAgentName);
        }
        if !names.insert(&agent.name) {
            return Err(WorkflowError::DuplicateAgentName(agent.name.clone()));
        }
        if agent.prompt.trim().is_empty() || agent.prompt.chars().count() > MAX_AGENT_PROMPT_CHARS {
            return Err(WorkflowError::InvalidAgentPrompt);
        }
    }
    Ok(())
}

fn herdr_agent_name(task: &Task, step: WorkflowStep, agent: &AgentPlan) -> String {
    let task_tag = task.id.0.simple().to_string();
    let step_tag = match step {
        WorkflowStep::SpecificationGeneration => "sg",
        WorkflowStep::CodeGeneration => "cg",
        WorkflowStep::CodeReview => "cr",
        WorkflowStep::Verification => "vf",
        WorkflowStep::Documentation => "dc",
        _ => unreachable!("only agent steps have Herdr agent names"),
    };
    let mut hasher = DefaultHasher::new();
    agent.name.hash(&mut hasher);
    let agent_hash = hasher.finish();
    let agent_tag = agent.name.chars().take(4).collect::<String>();
    // Keep the full hash while respecting Herdr's 32-character agent-name limit.
    format!(
        "c{}-{step_tag}-{agent_tag}-{agent_hash:016x}",
        &task_tag[..6]
    )
}

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::{AgentAdapter, AgentPlan, WorkflowEngine, pi_agent_args};
    use crate::{
        agent_profiles::{AgentProfile, AgentProfileCatalog, McpProfile},
        model::{Priority, Task, TaskStatus, WorkflowStep},
        project::{ProjectError, ProjectManager},
    };
    use std::{
        collections::HashSet,
        path::Path,
        sync::{
            Barrier, Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[test]
    fn agent_profiles_are_optional_in_existing_plan_json() {
        let legacy: AgentPlan =
            serde_json::from_str(r#"{"name":"reviewer","prompt":"review"}"#).unwrap();
        assert_eq!(legacy.profile_id, None);

        let configured: AgentPlan = serde_json::from_str(
            r#"{"name":"reviewer","prompt":"review","profile_id":"security-reviewer"}"#,
        )
        .unwrap();
        assert_eq!(configured.profile_id.as_deref(), Some("security-reviewer"));
    }

    #[test]
    fn pi_agent_arguments_load_only_selected_profile_resources() {
        let profile = AgentProfile {
            id: "security-reviewer".to_owned(),
            description: "Security review".to_owned(),
            steps: vec!["code-review".to_owned()],
            skills: vec![Path::new("/profiles/security skills/SKILL.md").to_path_buf()],
            extensions: vec![Path::new("/profiles/security extensions/index.ts").to_path_buf()],
            mcp: Some(McpProfile {
                adapter: Path::new("/profiles/mcp adapter").to_path_buf(),
                config: Path::new("/profiles/security mcp.json").to_path_buf(),
            }),
        };
        let args = pi_agent_args(Some(&profile))
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--no-skills",
                "--no-extensions",
                "--no-context-files",
                "--no-mcp",
                "--skill",
                "/profiles/security skills/SKILL.md",
                "--extension",
                "/profiles/security extensions/index.ts",
                "--extension",
                "/profiles/mcp adapter",
                "--mcp-config",
                "/profiles/security mcp.json",
            ]
        );
        assert_eq!(
            pi_agent_args(None)
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "--no-skills",
                "--no-extensions",
                "--no-context-files",
                "--no-mcp"
            ]
        );
    }

    struct ParallelAdapter {
        barrier: Barrier,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl AgentAdapter for ParallelAdapter {
        fn run(
            &self,
            _task: &Task,
            _step: WorkflowStep,
            _agent: &AgentPlan,
        ) -> Result<String, String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait();
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok("finished".to_owned())
        }

        fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
    }

    struct RequiresWorktreeAdapter {
        runs: AtomicUsize,
    }

    impl AgentAdapter for RequiresWorktreeAdapter {
        fn requires_worktree(&self) -> bool {
            true
        }

        fn run(
            &self,
            _task: &Task,
            _step: WorkflowStep,
            _agent: &AgentPlan,
        ) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok("should not execute".to_owned())
        }

        fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
    }

    struct CountingAdapter {
        runs: AtomicUsize,
    }

    impl AgentAdapter for CountingAdapter {
        fn run(
            &self,
            _task: &Task,
            _step: WorkflowStep,
            _agent: &AgentPlan,
        ) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok("unexpected run".to_owned())
        }

        fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
    }

    struct FailingAdapter {
        cancelled: Mutex<HashSet<String>>,
        ready: Condvar,
    }

    impl AgentAdapter for FailingAdapter {
        fn run(
            &self,
            _task: &Task,
            _step: WorkflowStep,
            agent: &AgentPlan,
        ) -> Result<String, String> {
            if agent.name == "failer" {
                return Err("review failed".to_owned());
            }
            let mut cancelled = self.cancelled.lock().unwrap();
            while !cancelled.contains(&agent.name) {
                cancelled = self.ready.wait(cancelled).unwrap();
            }
            Ok("late success must be ignored".to_owned())
        }

        fn cancel(&self, _task: &Task, _step: WorkflowStep, agent: &AgentPlan) {
            self.cancelled.lock().unwrap().insert(agent.name.clone());
            self.ready.notify_all();
        }
    }

    #[test]
    fn herdr_runner_refuses_missing_worktrees_before_creating_agents() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "No unscoped cwd".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        let adapter = RequiresWorktreeAdapter {
            runs: AtomicUsize::new(0),
        };
        let engine = WorkflowEngine::new(ProjectManager::new(root.path()).unwrap(), adapter);
        let result = engine.run_step(
            project.id,
            task.id,
            WorkflowStep::SpecificationGeneration,
            vec![AgentPlan {
                name: "spec-writer".to_owned(),
                prompt: "write specs".to_owned(),
                profile_id: None,
            }],
        );
        assert!(matches!(
            result,
            Err(super::WorkflowError::Project(
                ProjectError::InvalidTaskWorktree
            ))
        ));
        assert_eq!(engine.adapter.runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn invalid_agent_profiles_fail_before_activity_or_worker_launch() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Review change".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        let profiles = AgentProfileCatalog::from_profiles(vec![AgentProfile {
            id: "security-reviewer".to_owned(),
            description: "Checks security-sensitive changes".to_owned(),
            steps: vec!["code-review".to_owned()],
            skills: Vec::new(),
            extensions: Vec::new(),
            mcp: None,
        }])
        .unwrap();
        let adapter = CountingAdapter {
            runs: AtomicUsize::new(0),
        };
        let engine = WorkflowEngine::with_profiles(
            ProjectManager::new(root.path()).unwrap(),
            adapter,
            profiles,
        );
        let wrong_step = AgentPlan {
            name: "reviewer".to_owned(),
            prompt: "Review for security issues".to_owned(),
            profile_id: Some("security-reviewer".to_owned()),
        };
        assert!(matches!(
            engine.run_step(
                project.id,
                task.id,
                WorkflowStep::SpecificationGeneration,
                vec![wrong_step],
            ),
            Err(super::WorkflowError::InvalidAgentProfile { .. })
        ));
        let unknown = AgentPlan {
            name: "reviewer".to_owned(),
            prompt: "Review for security issues".to_owned(),
            profile_id: Some("missing-profile".to_owned()),
        };
        assert!(matches!(
            engine.run_step(
                project.id,
                task.id,
                WorkflowStep::SpecificationGeneration,
                vec![unknown],
            ),
            Err(super::WorkflowError::InvalidAgentProfile { .. })
        ));
        assert_eq!(engine.adapter.runs.load(Ordering::SeqCst), 0);
        assert!(
            manager
                .get_task(project.id, task.id)
                .unwrap()
                .agent_activity
                .is_empty()
        );
    }

    #[test]
    fn agent_failure_cancels_siblings_and_marks_task_error_without_advancing() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Cancel peers".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager
            .create_task(
                project.id,
                "Wait behind failure".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        let engine = WorkflowEngine::new(
            ProjectManager::new(root.path()).unwrap(),
            FailingAdapter {
                cancelled: Mutex::new(HashSet::new()),
                ready: Condvar::new(),
            },
        );
        let result = engine.run_step(
            project.id,
            task.id,
            WorkflowStep::SpecificationGeneration,
            vec![
                AgentPlan {
                    name: "failer".to_owned(),
                    prompt: "fail this step".to_owned(),
                    profile_id: None,
                },
                AgentPlan {
                    name: "worker".to_owned(),
                    prompt: "wait for cancellation".to_owned(),
                    profile_id: None,
                },
            ],
        );
        assert!(matches!(
            result,
            Err(super::WorkflowError::AgentFailed { .. })
        ));
        let stored = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Error);
        assert!(manager.start_next_task(project.id).unwrap().is_none());
        assert_eq!(
            stored.current_step.as_deref(),
            Some("specification-generation")
        );
        assert!(stored.agent_activity.iter().any(|event| {
            event.agent_name == "worker" && event.status == crate::model::AgentStatus::Cancelled
        }));
        assert!(!stored.agent_activity.iter().any(|event| {
            event.agent_name == "worker" && event.status == crate::model::AgentStatus::Completed
        }));
    }

    #[test]
    fn step_waits_for_every_agent_and_runs_agents_concurrently() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Parallel work".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        let engine = WorkflowEngine::new(
            ProjectManager::new(root.path()).unwrap(),
            ParallelAdapter {
                barrier: Barrier::new(2),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
            },
        );

        let updated = engine
            .run_step(
                project.id,
                task.id,
                WorkflowStep::SpecificationGeneration,
                vec![
                    AgentPlan {
                        name: "spec-writer".to_owned(),
                        prompt: "draft requirements".to_owned(),
                        profile_id: None,
                    },
                    AgentPlan {
                        name: "spec-reviewer".to_owned(),
                        prompt: "review requirements".to_owned(),
                        profile_id: None,
                    },
                ],
            )
            .unwrap();
        assert_eq!(
            updated.current_step.as_deref(),
            Some("specification-approval")
        );
        assert_eq!(updated.agent_activity.len(), 4);
        assert_eq!(engine.adapter.max_active.load(Ordering::SeqCst), 2);
    }
}
