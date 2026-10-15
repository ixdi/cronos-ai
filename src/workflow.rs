use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::Read,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread,
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;

use crate::{
    agent_profiles::{AgentProfile, AgentProfileCatalog},
    integrations::{GitAdapter, ToolCommands, ensure_real_directory, run_check_command},
    model::{AgentStatus, ProjectId, Task, TaskId, TaskStatus, WorkflowStep},
    project::{ProjectError, ProjectManager},
    sandbox::{SandboxPolicy, macos_profile, sandboxed_shell_command},
    step_skills::StepSkills,
};

const MAX_PARALLEL_AGENTS: usize = 4;
const MAX_AGENT_PROMPT_CHARS: usize = 20_000;
const MAX_AGENT_SUMMARY_CHARS: usize = 2_000;
const MAX_RESULT_BYTES: u64 = 64 * 1024;
const MAX_FINDINGS: usize = 20;
const MAX_FINDING_CHARS: usize = 1_000;
// shortcut: fixed check-command limit, make it configurable per project when a suite needs longer.
pub const RESULTS_DIR: &str = ".cronos/results";
pub const CRONOS_EXCLUDE_PATTERN: &str = ".cronos/";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentPlan {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub profile_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Success,
    ChangesRequested,
    Failed,
}

/// Structured result each agent writes to `<worktree>/.cronos/results/<step>/<agent>.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub status: ReportStatus,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

impl AgentReport {
    pub fn success(summary: impl Into<String>) -> Self {
        Self {
            status: ReportStatus::Success,
            summary: summary.into(),
            findings: Vec::new(),
        }
    }

    pub fn changes_requested(summary: impl Into<String>, findings: Vec<String>) -> Self {
        Self {
            status: ReportStatus::ChangesRequested,
            summary: summary.into(),
            findings,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() || self.summary.chars().count() > MAX_AGENT_SUMMARY_CHARS
        {
            return Err(format!(
                "result summary must be nonblank and at most {MAX_AGENT_SUMMARY_CHARS} characters"
            ));
        }
        if self.findings.len() > MAX_FINDINGS
            || self
                .findings
                .iter()
                .any(|finding| finding.chars().count() > MAX_FINDING_CHARS)
        {
            return Err(format!(
                "result findings are limited to {MAX_FINDINGS} items of {MAX_FINDING_CHARS} characters"
            ));
        }
        Ok(())
    }

    fn detail(&self) -> String {
        let mut detail = self.summary.clone();
        for finding in &self.findings {
            detail.push_str("\n- ");
            detail.push_str(finding);
        }
        truncate(&detail, MAX_AGENT_SUMMARY_CHARS)
    }
}

pub fn agent_result_path(worktree: &Path, step: WorkflowStep, agent: &str) -> PathBuf {
    worktree
        .join(RESULTS_DIR)
        .join(step.as_str())
        .join(format!("{agent}.json"))
}

/// Reads an agent result, rejecting symlinks, oversized files, and invalid JSON.
pub fn read_agent_report(path: &Path) -> Result<AgentReport, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| format!("agent did not write its result file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("agent result must be a regular file".to_owned());
    }
    if metadata.len() > MAX_RESULT_BYTES {
        return Err("agent result exceeds 64 KiB".to_owned());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_RESULT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("could not read agent result: {error}"))?;
    let report: AgentReport =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid agent result: {error}"))?;
    report.validate()?;
    Ok(report)
}

/// Creates `.cronos/results/<step>` in the worktree, refusing any symlinked component, so result
/// files are never removed or read through a link an agent placed there.
fn prepare_result_directory(worktree: &Path, step: WorkflowStep) -> Result<(), String> {
    let mut directory = worktree.to_path_buf();
    for component in [".cronos", "results", step.as_str()] {
        directory.push(component);
        ensure_real_directory(&directory)
            .map_err(|error| format!("unsafe agent result directory: {error}"))?;
    }
    Ok(())
}

fn remove_stale_result(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove stale agent result: {error}")),
    }
}

/// Agent limits per step: the specification has one author; code generation may split the work
/// between parallel writers in their own worktrees; review, verification, and docs run in parallel.
pub(crate) fn agent_count_range(step: WorkflowStep) -> (usize, usize) {
    match step {
        WorkflowStep::SpecificationGeneration => (1, 1),
        _ => (1, MAX_PARALLEL_AGENTS),
    }
}

/// Worktrees of parallel code-generation agents, each on a branch cut from the task branch. They
/// are removed when this value is dropped, whether the step succeeded or not.
struct WriterWorktrees {
    git: GitAdapter,
    task_worktree: PathBuf,
    parent: PathBuf,
    /// Agent name, worktree, and branch, sorted by agent name so merges are deterministic.
    writers: Vec<(String, PathBuf, String)>,
}

impl WriterWorktrees {
    fn create(
        git: &GitAdapter,
        task: &Task,
        task_worktree: &Path,
        agents: &[AgentPlan],
    ) -> Result<Self, String> {
        let name = task_worktree
            .file_name()
            .ok_or("the task worktree has no name")?
            .to_string_lossy();
        let parent = task_worktree
            .parent()
            .ok_or("the task worktree has no parent")?
            .join(format!("{name}-agents"));
        ensure_real_directory(&parent)
            .map_err(|error| format!("could not prepare writer worktrees: {error}"))?;
        let mut worktrees = Self {
            git: git.clone(),
            task_worktree: task_worktree.to_path_buf(),
            parent: parent.clone(),
            writers: Vec::new(),
        };
        let mut names = agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        for agent in names {
            let path = parent.join(&agent);
            // Git cannot nest a branch under the task branch's own name, so writers get a sibling.
            let branch = format!(
                "{}-agents/{agent}",
                GitAdapter::worktree_branch(task.id, task.attempt)
            );
            // A crashed earlier run may have left this writer's worktree behind.
            let _ = git.discard_attempt(task_worktree, &path, &branch);
            git.add_worktree(task_worktree, &path, &branch)
                .map_err(|error| format!("could not create {agent}'s worktree: {error}"))?;
            worktrees.writers.push((agent, path, branch));
        }
        Ok(worktrees)
    }

    fn path(&self, agent: &str) -> Option<&Path> {
        self.writers
            .iter()
            .find(|(name, _, _)| name == agent)
            .map(|(_, path, _)| path.as_path())
    }
}

impl Drop for WriterWorktrees {
    fn drop(&mut self) {
        for (_, path, branch) in &self.writers {
            let _ = self.git.discard_attempt(&self.task_worktree, path, branch);
        }
        let _ = std::fs::remove_dir(&self.parent);
    }
}

fn is_read_only(step: WorkflowStep) -> bool {
    matches!(step, WorkflowStep::CodeReview | WorkflowStep::Verification)
}

pub fn default_timeout_minutes(step: WorkflowStep) -> u32 {
    match step {
        WorkflowStep::CodeGeneration | WorkflowStep::Verification => 60,
        _ => 30,
    }
}

/// System-defined rules appended to every agent prompt, independent of what the orchestrator wrote.
pub fn agent_contract(
    task: &Task,
    step: WorkflowStep,
    agent: &str,
    worktree: &Path,
    writers: usize,
) -> String {
    let change = task.openspec_change();
    let rules = match step {
        WorkflowStep::CodeGeneration if writers > 1 => format!(
            "Implement your assigned part of the approved OpenSpec change `{change}` with tests. You are one of {writers} code-generation agents working in parallel, each in its own worktree on its own branch: change only the files your part needs. Cronos merges the agents' branches in name order into the task branch; overlapping edits conflict and send the task back to code generation. Commit with a Conventional Commit message and leave your worktree clean."
        ),
        WorkflowStep::SpecificationGeneration => format!(
            "Create or update the OpenSpec change `{change}` (proposal, design, specs, tasks) so `openspec validate {change} --strict` passes. Apply any revision note from the latest approval. Commit your changes with a Conventional Commit message and leave the worktree clean."
        ),
        WorkflowStep::CodeGeneration => format!(
            "Implement the approved OpenSpec change `{change}` with tests. If `openspec/changes/{change}` no longer exists, it was already archived under `openspec/changes/archive/` with its specs synced into `openspec/specs/`; treat those as the approved specification and do not recreate the change. Address findings from earlier review, verification, check-command, or merge-conflict activity and any revision note. You are the only writer in this step: commit with a Conventional Commit message and leave the worktree clean."
        ),
        WorkflowStep::CodeReview => format!(
            "Review the task branch against the OpenSpec change `{change}`. This step is read-only: do not modify, stage, or commit files. Report `changes_requested` with concrete findings when the code must change."
        ),
        WorkflowStep::Verification => format!(
            "Verify the task branch against the OpenSpec change `{change}` (tests, security, quality). This step is read-only: do not modify, stage, or commit files. Cronos runs the project's check command after you. Report `changes_requested` with concrete findings when the code must change."
        ),
        WorkflowStep::Documentation => format!(
            "Write or update documentation for the OpenSpec change `{change}`. Edit only the files your role owns and do not commit or archive; Cronos archives the change and makes the documentation commit."
        ),
        _ => String::new(),
    };
    format!(
        "\n\n---\nCronos-AI contract for agent `{agent}` in step `{step}` (task {task_id}, attempt {attempt}):\n- {rules}\n- Before you finish, write your result as JSON to `{path}`: {{\"status\": \"success\" | \"changes_requested\" | \"failed\", \"summary\": \"...\", \"findings\": [\"...\"]}}. Use `failed` only when you cannot complete the role. `changes_requested` is valid only in code review and verification.\n- Work only inside `{worktree}`. Never push, never edit Cronos-AI data files, and never change task status or approvals.",
        step = step.as_str(),
        task_id = task.id.0,
        attempt = task.attempt,
        path = agent_result_path(worktree, step, agent).display(),
        worktree = worktree.display(),
    )
}

#[derive(Default)]
pub struct HerdrAgentAdapter {
    tools: ToolCommands,
    pane_creation: Mutex<()>,
    profiles: AgentProfileCatalog,
    step_skills: StepSkills,
    /// When set, every worker's pane shell is replaced with a sandboxed one before Pi starts.
    sandbox: Option<SandboxPolicy>,
    /// Herdr names of agents whose cancellation was requested. Checked before every launch stage,
    /// so a sibling that has not started yet never starts after its step failed.
    cancelled: Mutex<HashSet<String>>,
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
            step_skills: StepSkills::default(),
            sandbox: None,
            cancelled: Mutex::new(HashSet::new()),
        }
    }

    /// Runs every worker inside the write sandbox described by `policy`.
    pub fn with_sandbox(mut self, policy: SandboxPolicy) -> Self {
        self.sandbox = Some(policy);
        self
    }

    /// Replaces the pane's shell with a sandboxed shell for a worker in `worktree`.
    fn enter_sandbox(
        &self,
        herdr_name: &str,
        pane_id: &str,
        worktree: &Path,
    ) -> Result<(), String> {
        let Some(policy) = &self.sandbox else {
            return Ok(());
        };
        let directory = ensure_real_directory(&policy.data_dir.join("sandbox"))
            .map_err(|error| format!("could not prepare the sandbox: {error}"))?;
        let worktree = std::fs::canonicalize(worktree)
            .map_err(|error| format!("could not resolve the worktree: {error}"))?;
        let profile = directory.join(format!("{herdr_name}.sb"));
        std::fs::write(&profile, macos_profile(policy, &worktree))
            .map_err(|error| format!("could not write the sandbox profile: {error}"))?;
        let shell = std::env::var_os("SHELL")
            .filter(|shell| Path::new(shell).is_absolute())
            .map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from);
        let command = sandboxed_shell_command(policy, &worktree, &profile, &shell)?;
        self.tools
            .herdr_run_in_pane(pane_id, &command)
            .map(|_| ())
            .map_err(|error| format!("could not start the sandboxed shell: {error}"))
    }

    /// Gives every worker the installed built-in skill for its step.
    pub fn with_step_skills(mut self, step_skills: StepSkills) -> Self {
        self.step_skills = step_skills;
        self
    }

    fn is_cancelled(&self, herdr_name: &str) -> bool {
        self.cancelled
            .lock()
            .map_or(true, |cancelled| cancelled.contains(herdr_name))
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_and_wait(
        &self,
        herdr_name: &str,
        pane_id: &str,
        pi_args: &[OsString],
        prompt: &str,
        timeout_minutes: u32,
        worktree: &Path,
        step: WorkflowStep,
        agent: &str,
    ) -> Result<AgentReport, String> {
        let cancelled = || Err("agent was cancelled because a sibling failed".to_owned());
        if self.is_cancelled(herdr_name) {
            return cancelled();
        }
        self.tools
            .herdr_start_pi_agent(herdr_name, pane_id, pi_args)
            .map_err(|error| error.to_string())?;
        // A cancel that arrived while the agent was starting found nothing to stop; check again.
        if self.is_cancelled(herdr_name) {
            return cancelled();
        }
        if let Err(error) = self.tools.herdr_prompt_agent(
            herdr_name,
            prompt,
            Duration::from_secs(u64::from(timeout_minutes) * 60),
        ) {
            if self.is_cancelled(herdr_name) {
                return cancelled();
            }
            return Err(self.fail_with_output(
                herdr_name,
                format!("agent failed or exceeded its {timeout_minutes}-minute limit: {error}"),
            ));
        }
        if self.is_cancelled(herdr_name) {
            return cancelled();
        }
        let status = self
            .tools
            .herdr_agent_status(herdr_name)
            .map_err(|error| error.to_string())?;
        match status.as_str() {
            "idle" | "done" => {}
            "blocked" => {
                return Err(self.fail_with_output(
                    herdr_name,
                    "agent is blocked on an interactive prompt".to_owned(),
                ));
            }
            status => return Err(format!("agent did not settle; status is {status}")),
        }
        prepare_result_directory(worktree, step)?;
        read_agent_report(&agent_result_path(worktree, step, agent))
            .map_err(|error| self.fail_with_output(herdr_name, error))
    }

    fn fail_with_output(&self, herdr_name: &str, message: String) -> String {
        let output = self
            .tools
            .herdr_read_agent(herdr_name, 20)
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default();
        if output.is_empty() {
            message
        } else {
            truncate(
                &format!("{message}\nLast agent output:\n{output}"),
                MAX_AGENT_SUMMARY_CHARS,
            )
        }
    }
}

impl AgentAdapter for HerdrAgentAdapter {
    fn requires_worktree(&self) -> bool {
        true
    }

    fn run(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String> {
        let worktree = task
            .worktree
            .as_deref()
            .ok_or_else(|| "task worktree is not set".to_owned())?;
        let worktree = Path::new(worktree);
        if !worktree.is_absolute() || !worktree.is_dir() {
            return Err("task worktree is unavailable".to_owned());
        }
        let result_path = agent_result_path(worktree, step, &agent.name);
        prepare_result_directory(worktree, step)?;
        remove_stale_result(&result_path)?;
        let herdr_name = herdr_agent_name(task, step, &agent.name);
        let profile = agent
            .profile_id
            .as_deref()
            .and_then(|id| self.profiles.get_for_step(id, step));
        let timeout_minutes = profile
            .and_then(|profile| profile.timeout_minutes)
            .unwrap_or_else(|| default_timeout_minutes(step));
        let pi_args = pi_agent_args(self.step_skills.path(step), profile);
        let pane_id = {
            let _guard = self
                .pane_creation
                .lock()
                .map_err(|_| "Herdr pane allocator is unavailable".to_owned())?;
            if self.is_cancelled(&herdr_name) {
                return Err("agent was cancelled because a sibling failed".to_owned());
            }
            self.tools
                .herdr_split_pane(worktree)
                .map_err(|error| error.to_string())?
        };
        let result = self
            .enter_sandbox(&herdr_name, &pane_id, worktree)
            .and_then(|()| {
                self.launch_and_wait(
                    &herdr_name,
                    &pane_id,
                    &pi_args,
                    &agent.prompt,
                    timeout_minutes,
                    worktree,
                    step,
                    &agent.name,
                )
            });
        if result.is_err() {
            let _ = self.tools.herdr_cancel_agent(&herdr_name);
        }
        // Closing the pane also ends the agent session; output for failures was captured above.
        let _ = self.tools.herdr_close_pane(&pane_id);
        result
    }

    fn cancel(&self, task: &Task, step: WorkflowStep, agent: &AgentPlan) {
        let herdr_name = herdr_agent_name(task, step, &agent.name);
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.insert(herdr_name.clone());
        }
        let _ = self.tools.herdr_cancel_agent(&herdr_name);
    }

    fn begin_step(&self) {
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.clear();
        }
    }
}

/// Stops an agent left behind by an earlier process and closes its pane.
fn cancel_stale_herdr_agent(tools: &ToolCommands, task: &Task, step: WorkflowStep, agent: &str) {
    let herdr_name = herdr_agent_name(task, step, agent);
    let pane = tools.herdr_agent_pane(&herdr_name);
    let _ = tools.herdr_cancel_agent(&herdr_name);
    if let Ok(pane_id) = pane {
        let _ = tools.herdr_close_pane(&pane_id);
    }
}

pub trait AgentAdapter: Send + Sync {
    /// Production adapters run in a Git worktree, which also enables the worktree checks.
    fn requires_worktree(&self) -> bool {
        false
    }

    fn run(
        &self,
        task: &Task,
        step: WorkflowStep,
        agent: &AgentPlan,
    ) -> Result<AgentReport, String>;

    fn cancel(&self, task: &Task, step: WorkflowStep, agent: &AgentPlan);

    /// Called before a step's agents start; adapters reset per-step state here.
    fn begin_step(&self) {}
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error("step {0:?} is not an agent step")]
    NotAgentStep(WorkflowStep),
    #[error("step {step:?} requires {min} to {max} agents")]
    InvalidAgentCount {
        step: WorkflowStep,
        min: usize,
        max: usize,
    },
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
    #[error("workflow check failed: {0}")]
    StepCheckFailed(String),
}

pub struct WorkflowEngine<A> {
    manager: ProjectManager,
    adapter: A,
    profiles: AgentProfileCatalog,
    tools: ToolCommands,
}

impl<A: AgentAdapter> WorkflowEngine<A> {
    pub fn new(manager: ProjectManager, adapter: A) -> Self {
        Self::with_tools(
            manager,
            adapter,
            AgentProfileCatalog::default(),
            ToolCommands::default(),
        )
    }

    pub fn with_profiles(
        manager: ProjectManager,
        adapter: A,
        profiles: AgentProfileCatalog,
    ) -> Self {
        Self::with_tools(manager, adapter, profiles, ToolCommands::default())
    }

    pub fn with_tools(
        manager: ProjectManager,
        adapter: A,
        profiles: AgentProfileCatalog,
        tools: ToolCommands,
    ) -> Self {
        Self {
            manager,
            adapter,
            profiles,
            tools,
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
        validate_agents(step, &agents)?;
        validate_profile_assignments(&self.profiles, step, &agents)?;
        // One step process per task; recovery actions wait for this lock to be released.
        let _lock = self.manager.lock_task(task_id)?;
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
        let worktree = if self.adapter.requires_worktree() {
            let path = self
                .manager
                .validated_task_worktree_path(project_id, task_id)?;
            task.worktree = Some(path.to_string_lossy().into_owned());
            Some(path)
        } else {
            None
        };
        let git = GitAdapter::with_tools(self.tools.clone());
        let before = match (&worktree, is_read_only(step)) {
            (Some(path), true) => Some(git.snapshot(path).map_err(|error| {
                self.fail_step(
                    project_id,
                    task_id,
                    step,
                    format!("could not inspect the worktree: {error}"),
                )
            })?),
            _ => None,
        };

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
        let writers = match &worktree {
            Some(path) if step == WorkflowStep::CodeGeneration && agents.len() > 1 => Some(
                WriterWorktrees::create(&git, &task, path, &agents)
                    .map_err(|error| self.fail_step(project_id, task_id, step, error))?,
            ),
            _ => None,
        };
        let agent_worktree = |name: &str| -> Option<PathBuf> {
            writers
                .as_ref()
                .and_then(|writers| writers.path(name))
                .map(Path::to_path_buf)
                .or_else(|| worktree.clone())
        };
        let launch_plans = agents
            .iter()
            .map(|agent| AgentPlan {
                prompt: match agent_worktree(&agent.name) {
                    Some(path) => format!(
                        "{}{}",
                        agent.prompt,
                        agent_contract(&task, step, &agent.name, &path, agents.len())
                    ),
                    None => agent.prompt.clone(),
                },
                ..agent.clone()
            })
            .collect::<Vec<_>>();
        let overrides = agents
            .iter()
            .filter_map(|agent| {
                writers
                    .as_ref()
                    .and_then(|writers| writers.path(&agent.name))
                    .map(|path| (agent.name.clone(), path.to_path_buf()))
            })
            .collect::<HashMap<_, _>>();

        let outcome = run_parallel(&self.adapter, &task, step, &launch_plans, &overrides);
        // A human may have stopped or recovered the task while agents ran; never apply stale results.
        let current = self.manager.get_task(project_id, task_id)?;
        if current.attempt != task.attempt
            || current.status != TaskStatus::InProgress
            || current.current_step.as_deref() != Some(step.as_str())
        {
            return Err(WorkflowError::StepCheckFailed(format!(
                "task changed while {} ran (now attempt {}, {:?}); results were discarded",
                step.as_str(),
                current.attempt,
                current.status
            )));
        }
        let results = match outcome {
            Ok(results) => results,
            Err(failure) => {
                for (agent, report) in failure.completed {
                    self.record(project_id, task_id, step, agent, &report)?;
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
                return Err(WorkflowError::AgentFailed {
                    name: failure.name,
                    message: failure.message,
                });
            }
        };
        let mut changes_requested = false;
        for (agent, report) in results {
            changes_requested |= report.status == ReportStatus::ChangesRequested;
            self.record(project_id, task_id, step, agent, &report)?;
        }
        if let (Some(writers), Some(task_worktree)) = (&writers, &worktree)
            && let Some(conflict) =
                self.merge_writers(project_id, task_id, writers, task_worktree, &git)?
        {
            self.manager.record_agent_activity(
                project_id,
                task_id,
                "cronos".to_owned(),
                step.as_str().to_owned(),
                AgentStatus::ChangesRequested,
                Some(truncate(&conflict, MAX_AGENT_SUMMARY_CHARS)),
            )?;
            return Ok(self
                .manager
                .request_automatic_revision(project_id, task_id)?);
        }

        if let Some(worktree) = &worktree {
            self.check_worktree(project_id, &task, step, worktree, &git, before.as_deref())?;
            if step == WorkflowStep::Verification && !changes_requested {
                changes_requested = self.run_project_check(project_id, task_id, worktree)?;
            }
        }
        if changes_requested {
            return Ok(self
                .manager
                .request_automatic_revision(project_id, task_id)?);
        }
        Ok(self
            .manager
            .complete_workflow_step(project_id, task_id, step)?)
    }

    /// Merges the parallel writers' branches into the task branch in name order. On a conflict the
    /// task branch is reset to where it started and the conflict is returned as a finding.
    fn merge_writers(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        writers: &WriterWorktrees,
        task_worktree: &Path,
        git: &GitAdapter,
    ) -> Result<Option<String>, WorkflowError> {
        let step = WorkflowStep::CodeGeneration;
        let fail = |message: String| self.fail_step(project_id, task_id, step, message);
        let start = git
            .head(task_worktree)
            .map_err(|error| fail(error.to_string()))?;
        for (agent, path, branch) in &writers.writers {
            if !git
                .is_clean(path)
                .map_err(|error| fail(error.to_string()))?
            {
                let _ = git.reset_hard(task_worktree, &start);
                return Err(fail(format!(
                    "code-generation agent {agent} left uncommitted changes; each writer must commit its work"
                )));
            }
            match git.merge_agent(
                task_worktree,
                path,
                branch,
                &format!("Merge code-generation agent {agent}"),
            ) {
                Ok(()) => {}
                Err(crate::integrations::GitError::MergeConflict(detail)) => {
                    git.reset_hard(task_worktree, &start)
                        .map_err(|error| fail(error.to_string()))?;
                    return Ok(Some(format!(
                        "The parallel code-generation agents' changes conflicted while merging {agent}, so none were kept. Give each agent a separate part of the code, or use one agent. {detail}"
                    )));
                }
                Err(error) => {
                    let _ = git.reset_hard(task_worktree, &start);
                    return Err(fail(format!("could not merge {agent}'s work: {error}")));
                }
            }
        }
        Ok(None)
    }

    fn record(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        step: WorkflowStep,
        agent: String,
        report: &AgentReport,
    ) -> Result<(), WorkflowError> {
        let status = match report.status {
            ReportStatus::ChangesRequested => AgentStatus::ChangesRequested,
            _ => AgentStatus::Completed,
        };
        self.manager.record_agent_activity(
            project_id,
            task_id,
            agent,
            step.as_str().to_owned(),
            status,
            Some(report.detail()),
        )?;
        Ok(())
    }

    /// Records a failed system check on the step and marks the task Error.
    fn fail_step(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        step: WorkflowStep,
        message: String,
    ) -> WorkflowError {
        let _ = self.manager.record_agent_activity(
            project_id,
            task_id,
            "cronos".to_owned(),
            step.as_str().to_owned(),
            AgentStatus::Failed,
            Some(truncate(&message, MAX_AGENT_SUMMARY_CHARS)),
        );
        if let Err(error) = self
            .manager
            .set_task_status(project_id, task_id, TaskStatus::Error)
        {
            return error.into();
        }
        WorkflowError::StepCheckFailed(message)
    }

    fn check_worktree(
        &self,
        project_id: ProjectId,
        task: &Task,
        step: WorkflowStep,
        worktree: &Path,
        git: &GitAdapter,
        before: Option<&str>,
    ) -> Result<(), WorkflowError> {
        let fail = |message: String| Err(self.fail_step(project_id, task.id, step, message));
        let git_error = |error: crate::integrations::GitError| {
            self.fail_step(project_id, task.id, step, error.to_string())
        };
        if let Some(before) = before
            && git.snapshot(worktree).map_err(git_error)? != before
        {
            return fail(format!(
                "{} is read-only, but its agents changed the worktree",
                step.as_str()
            ));
        }
        let change = task.openspec_change();
        match step {
            WorkflowStep::SpecificationGeneration | WorkflowStep::CodeGeneration => {
                if !git.is_clean(worktree).map_err(git_error)? {
                    return fail(format!(
                        "{} left uncommitted changes; the agent must commit its work",
                        step.as_str()
                    ));
                }
                if step == WorkflowStep::SpecificationGeneration
                    && let Err(error) = self.tools.run_openspec_in(
                        worktree,
                        &["validate", &change, "--strict", "--no-interactive"],
                    )
                {
                    return fail(format!(
                        "OpenSpec change {change} is missing or invalid: {error}"
                    ));
                }
            }
            WorkflowStep::Documentation => {
                if worktree.join("openspec/changes").join(&change).is_dir()
                    && let Err(error) = self
                        .tools
                        .run_openspec_in(worktree, &["archive", &change, "--yes"])
                {
                    return fail(format!(
                        "could not archive OpenSpec change {change}: {error}"
                    ));
                }
                if !git.is_clean(worktree).map_err(git_error)?
                    && let Err(error) = git.commit_all(
                        worktree,
                        &format!("docs: document {}", commit_subject(&task.title)),
                    )
                {
                    return fail(format!("could not commit documentation: {error}"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Runs the project check command; returns true when it failed and the code needs revision.
    fn run_project_check(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        worktree: &Path,
    ) -> Result<bool, WorkflowError> {
        let step = WorkflowStep::Verification;
        let project = self.manager.get_project(project_id)?;
        let timeout = project.check_timeout();
        let Some(command) = project.check_command else {
            return Err(self.fail_step(
                project_id,
                task_id,
                step,
                "the project has no check command; configure one before verification".to_owned(),
            ));
        };
        let git = GitAdapter::with_tools(self.tools.clone());
        let warning = project
            .repository
            .as_deref()
            .and_then(|repository| check_program_changed(&git, Path::new(repository), worktree, &command))
            .map(|program| format!("WARNING: the task branch changes the check program {program}; review it before final approval.\n"))
            .unwrap_or_default();
        let before = git
            .status(worktree)
            .map_err(|error| self.fail_step(project_id, task_id, step, error.to_string()))?;
        let outcome = run_check_command(worktree, &command, timeout)
            .map_err(|error| self.fail_step(project_id, task_id, step, error.to_string()))?;
        let after = git
            .status(worktree)
            .map_err(|error| self.fail_step(project_id, task_id, step, error.to_string()))?;
        // Files the check leaves behind would end up in the documentation commit; send them back
        // to code generation so they are ignored or no longer produced.
        let leftovers = (after != before).then(|| {
            let before = before.lines().collect::<HashSet<_>>();
            after
                .lines()
                .filter(|line| !before.contains(line))
                .collect::<Vec<_>>()
                .join("\n")
        });
        let success = outcome.success && leftovers.is_none();
        let detail = match (&leftovers, outcome.success) {
            (_, false) => format!("{warning}check command failed:\n{}", outcome.output_tail),
            (Some(files), true) => format!(
                "{warning}check command passed but left changes in the worktree; add them to .gitignore or stop producing them:\n{files}"
            ),
            (None, true) => format!("{warning}check command passed"),
        };
        self.manager.record_agent_activity(
            project_id,
            task_id,
            "check-command".to_owned(),
            step.as_str().to_owned(),
            if success {
                AgentStatus::Completed
            } else {
                AgentStatus::ChangesRequested
            },
            Some(if success {
                truncate(&detail, MAX_AGENT_SUMMARY_CHARS)
            } else {
                truncate_tail(&detail, MAX_AGENT_SUMMARY_CHARS)
            }),
        )?;
        Ok(!success)
    }
}

/// Stops this attempt's unfinished agents in the current step, closes their panes, and, while the
/// task is In Progress, records them as cancelled. Used before resume, step retry, and retry.
/// Fails with `TaskBusy` while another process is still running a step for the task.
pub fn cancel_interrupted_agents(
    manager: &ProjectManager,
    tools: &ToolCommands,
    project_id: ProjectId,
    task_id: TaskId,
) -> Result<usize, WorkflowError> {
    let _lock = manager.lock_task(task_id)?;
    let task = manager.get_task(project_id, task_id)?;
    let Some(step) = task.current_step.as_deref().and_then(WorkflowStep::parse) else {
        return Ok(0);
    };
    let interrupted = manager.interrupted_agents(project_id, task_id)?;
    for agent in &interrupted {
        if step.runs_agents() {
            cancel_stale_herdr_agent(tools, &task, step, agent);
        }
        if task.status == TaskStatus::InProgress {
            manager.record_agent_activity(
                project_id,
                task_id,
                agent.clone(),
                step.as_str().to_owned(),
                AgentStatus::Cancelled,
                Some("interrupted; cancelled on resume".to_owned()),
            )?;
        }
    }
    Ok(interrupted.len())
}

struct ParallelFailure {
    name: String,
    message: String,
    completed: Vec<(String, AgentReport)>,
    cancelled: Vec<String>,
}

/// Runs every agent at once. An agent listed in `worktrees` works in that worktree instead of
/// the task's.
fn run_parallel<A: AgentAdapter>(
    adapter: &A,
    task: &Task,
    step: WorkflowStep,
    agents: &[AgentPlan],
    worktrees: &HashMap<String, PathBuf>,
) -> Result<Vec<(String, AgentReport)>, ParallelFailure> {
    adapter.begin_step();
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for agent in agents.iter().cloned() {
            let sender = sender.clone();
            let mut task = task.clone();
            if let Some(path) = worktrees.get(&agent.name) {
                task.worktree = Some(path.to_string_lossy().into_owned());
            }
            scope.spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| adapter.run(&task, step, &agent)))
                    .unwrap_or_else(|_| Err("agent worker panicked".to_owned()))
                    .and_then(|report| match report.status {
                        ReportStatus::Failed => Err(format!(
                            "agent reported failure: {}",
                            report.detail()
                        )),
                        ReportStatus::ChangesRequested if !is_read_only(step) => Err(format!(
                            "changes_requested is only valid in code review or verification, not {}",
                            step.as_str()
                        )),
                        _ => Ok(report),
                    });
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
                (true, Ok(report)) => {
                    completed.insert(name, report);
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
                        .map(|report| (agent.name.clone(), report))
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

/// The check program when it lives in the repository and the task branch committed a different
/// version than the base. The check then runs code the agents wrote, so the human must look.
pub fn check_program_changed(
    git: &GitAdapter,
    repository: &Path,
    worktree: &Path,
    command: &[String],
) -> Option<String> {
    let program = Path::new(command.first()?);
    if program.is_absolute() || program.components().count() < 2 {
        return None;
    }
    let file = program
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let base = git.committed_blob(repository, &file);
    let task = git.committed_blob(worktree, &file);
    let uncommitted = git
        .status(worktree)
        .is_ok_and(|status| status.lines().any(|line| line.ends_with(&file)));
    (base != task || uncommitted).then_some(file)
}

/// Pi arguments for a worker: built-in resources off, then the step's built-in skill, then only
/// what the agent's profile assigns.
fn pi_agent_args(step_skill: Option<&Path>, profile: Option<&AgentProfile>) -> Vec<OsString> {
    let mut args = [
        "--no-skills",
        "--no-extensions",
        "--no-context-files",
        "--no-mcp",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();

    if let Some(model) = profile.and_then(|profile| profile.model.as_ref()) {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(skill) = step_skill {
        args.push("--skill".into());
        args.push(skill.as_os_str().to_owned());
    }
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

pub(crate) fn validate_profile_assignments(
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

pub(crate) fn validate_agents(
    step: WorkflowStep,
    agents: &[AgentPlan],
) -> Result<(), WorkflowError> {
    let (min, max) = agent_count_range(step);
    if agents.len() < min || agents.len() > max {
        return Err(WorkflowError::InvalidAgentCount { step, min, max });
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

fn herdr_agent_name(task: &Task, step: WorkflowStep, agent: &str) -> String {
    let task_tag = task.id.0.simple().to_string();
    let step_tag = match step {
        WorkflowStep::SpecificationGeneration => "sg",
        WorkflowStep::CodeGeneration => "cg",
        WorkflowStep::CodeReview => "cr",
        WorkflowStep::Verification => "vf",
        WorkflowStep::Documentation => "dc",
        _ => unreachable!("only agent steps have Herdr agent names"),
    };
    // The attempt is part of the name so a new attempt never reuses agents left by an older one.
    // FNV-1a keeps names stable across builds, which resuming after an upgrade relies on.
    let agent_hash = fnv1a64(format!("{task_tag}/{}/{agent}", task.attempt).as_bytes());
    let agent_tag = agent.chars().take(4).collect::<String>();
    // Keep the full hash while respecting Herdr's 32-character agent-name limit.
    format!(
        "c{}-{step_tag}-{agent_tag}-{agent_hash:016x}",
        &task_tag[..6]
    )
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

/// One-line commit subject from user-provided task text.
fn commit_subject(title: &str) -> String {
    let subject = title.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&subject, 72)
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn truncate_tail(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    value
        .chars()
        .skip(count.saturating_sub(max_chars))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        AgentAdapter, AgentPlan, AgentReport, ReportStatus, WorkflowEngine, agent_contract,
        agent_result_path, pi_agent_args, read_agent_report,
    };
    use crate::{
        agent_profiles::{AgentProfile, AgentProfileCatalog, McpProfile},
        model::{
            AgentStatus, ApprovalDecision, ApprovalGate, Priority, ProjectId, Task, TaskId,
            TaskStatus, WorkflowStep,
        },
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
            model: Some("anthropic/claude-sonnet-4".to_owned()),
            timeout_minutes: Some(20),
        };
        let step_skill = Path::new("/data/skills/cronos-code-review/SKILL.md");
        let args = pi_agent_args(Some(step_skill), Some(&profile))
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
                "--model",
                "anthropic/claude-sonnet-4",
                "--skill",
                "/data/skills/cronos-code-review/SKILL.md",
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
            pi_agent_args(None, None)
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
        ) -> Result<AgentReport, String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait();
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(AgentReport::success("finished"))
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
        ) -> Result<AgentReport, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(AgentReport::success("should not execute"))
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
        ) -> Result<AgentReport, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(AgentReport::success("unexpected run"))
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
        ) -> Result<AgentReport, String> {
            if agent.name == "failer" {
                return Err("review failed".to_owned());
            }
            let mut cancelled = self.cancelled.lock().unwrap();
            while !cancelled.contains(&agent.name) {
                cancelled = self.ready.wait(cancelled).unwrap();
            }
            Ok(AgentReport::success("late success must be ignored"))
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
            model: None,
            timeout_minutes: None,
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
        move_to_code_review(&manager, project.id, task.id);
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
            WorkflowStep::CodeReview,
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
        assert_eq!(stored.current_step.as_deref(), Some("code-review"));
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
        move_to_code_review(&manager, project.id, task.id);
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
                WorkflowStep::CodeReview,
                vec![
                    AgentPlan {
                        name: "quality-reviewer".to_owned(),
                        prompt: "review quality".to_owned(),
                        profile_id: None,
                    },
                    AgentPlan {
                        name: "security-reviewer".to_owned(),
                        prompt: "review security".to_owned(),
                        profile_id: None,
                    },
                ],
            )
            .unwrap();
        assert_eq!(updated.current_step.as_deref(), Some("verification"));
        assert_eq!(updated.agent_activity.len(), 4);
        assert_eq!(engine.adapter.max_active.load(Ordering::SeqCst), 2);
    }

    fn move_to_code_review(manager: &ProjectManager, project_id: ProjectId, task_id: TaskId) {
        manager
            .complete_workflow_step(project_id, task_id, WorkflowStep::SpecificationGeneration)
            .unwrap();
        manager
            .record_approval(
                project_id,
                task_id,
                ApprovalGate::Specifications,
                ApprovalDecision::Approved,
                None,
            )
            .unwrap();
        manager
            .complete_workflow_step(project_id, task_id, WorkflowStep::CodeGeneration)
            .unwrap();
    }

    fn plan(name: &str) -> AgentPlan {
        AgentPlan {
            name: name.to_owned(),
            prompt: format!("act as {name}"),
            profile_id: None,
        }
    }

    /// Returns a fixed report for every agent.
    struct ReportingAdapter(AgentReport);

    impl AgentAdapter for ReportingAdapter {
        fn run(
            &self,
            _task: &Task,
            _step: WorkflowStep,
            _agent: &AgentPlan,
        ) -> Result<AgentReport, String> {
            Ok(self.0.clone())
        }

        fn cancel(&self, _task: &Task, _step: WorkflowStep, _agent: &AgentPlan) {}
    }

    fn started_task(root: &Path) -> (ProjectManager, ProjectId, TaskId) {
        let manager = ProjectManager::new(root).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
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
        (manager, project.id, task.id)
    }

    #[test]
    fn the_specification_accepts_exactly_one_author() {
        let root = tempfile::tempdir().unwrap();
        let (manager, project_id, task_id) = started_task(root.path());
        let engine = WorkflowEngine::new(
            ProjectManager::new(root.path()).unwrap(),
            ReportingAdapter(AgentReport::success("done")),
        );
        assert!(matches!(
            engine.run_step(
                project_id,
                task_id,
                WorkflowStep::SpecificationGeneration,
                vec![plan("writer"), plan("second-writer")],
            ),
            Err(super::WorkflowError::InvalidAgentCount { min: 1, max: 1, .. })
        ));
        assert!(
            manager
                .get_task(project_id, task_id)
                .unwrap()
                .agent_activity
                .is_empty()
        );
    }

    #[test]
    fn review_changes_request_a_bounded_automatic_revision() {
        let root = tempfile::tempdir().unwrap();
        let (manager, project_id, task_id) = started_task(root.path());
        move_to_code_review(&manager, project_id, task_id);
        let engine = WorkflowEngine::new(
            ProjectManager::new(root.path()).unwrap(),
            ReportingAdapter(AgentReport::changes_requested(
                "needs work",
                vec!["handle the empty input".to_owned()],
            )),
        );
        for revision in 1..=crate::project::MAX_AUTOMATIC_REVISIONS {
            let task = engine
                .run_step(
                    project_id,
                    task_id,
                    WorkflowStep::CodeReview,
                    vec![plan("reviewer")],
                )
                .unwrap();
            assert_eq!(task.status, TaskStatus::InProgress);
            assert_eq!(task.current_step.as_deref(), Some("code-generation"));
            assert_eq!(task.automatic_revisions, revision);
            let findings = task.agent_activity.last().unwrap();
            assert_eq!(findings.status, AgentStatus::ChangesRequested);
            assert!(findings.detail.as_deref().unwrap().contains("empty input"));
            manager
                .complete_workflow_step(project_id, task_id, WorkflowStep::CodeGeneration)
                .unwrap();
        }
        assert!(matches!(
            engine.run_step(
                project_id,
                task_id,
                WorkflowStep::CodeReview,
                vec![plan("reviewer")]
            ),
            Err(super::WorkflowError::Project(
                ProjectError::AutomaticRevisionLimit
            ))
        ));
        let stopped = manager.get_task(project_id, task_id).unwrap();
        assert_eq!(stopped.status, TaskStatus::Error);
        // The reason is recorded where the human looks.
        assert!(stopped.agent_activity.last().is_some_and(|event| {
            event.status == AgentStatus::Failed
                && event
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("revision limit"))
        }));
    }

    #[test]
    fn failed_or_misplaced_reports_mark_the_task_error() {
        for (report, step) in [
            (
                AgentReport {
                    status: ReportStatus::Failed,
                    summary: "could not build".to_owned(),
                    findings: Vec::new(),
                },
                WorkflowStep::CodeReview,
            ),
            (
                AgentReport::changes_requested("not valid here", Vec::new()),
                WorkflowStep::SpecificationGeneration,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (manager, project_id, task_id) = started_task(root.path());
            if step == WorkflowStep::CodeReview {
                move_to_code_review(&manager, project_id, task_id);
            }
            let engine = WorkflowEngine::new(
                ProjectManager::new(root.path()).unwrap(),
                ReportingAdapter(report),
            );
            assert!(matches!(
                engine.run_step(project_id, task_id, step, vec![plan("agent")]),
                Err(super::WorkflowError::AgentFailed { .. })
            ));
            let task = manager.get_task(project_id, task_id).unwrap();
            assert_eq!(task.status, TaskStatus::Error);
            assert_eq!(task.current_step.as_deref(), Some(step.as_str()));
        }
    }

    #[test]
    fn agent_results_are_strictly_parsed() {
        let root = tempfile::tempdir().unwrap();
        let path = agent_result_path(root.path(), WorkflowStep::CodeReview, "reviewer");
        assert!(path.ends_with(".cronos/results/code-review/reviewer.json"));
        assert!(
            read_agent_report(&path)
                .unwrap_err()
                .contains("did not write")
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(
            &path,
            r#"{"status":"changes_requested","summary":"fix it","findings":["a"]}"#,
        )
        .unwrap();
        let report = read_agent_report(&path).unwrap();
        assert_eq!(report.status, ReportStatus::ChangesRequested);
        assert_eq!(report.findings, ["a"]);

        for invalid in [
            r#"{"status":"success","summary":"ok","extra":1}"#,
            r#"{"status":"maybe","summary":"ok"}"#,
            r#"{"status":"success","summary":"  "}"#,
            "not json",
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert!(read_agent_report(&path).is_err(), "{invalid}");
        }
        std::fs::write(&path, vec![b' '; 65 * 1024]).unwrap();
        assert!(read_agent_report(&path).unwrap_err().contains("64 KiB"));

        #[cfg(unix)]
        {
            let target = root.path().join("elsewhere.json");
            std::fs::write(&target, r#"{"status":"success","summary":"ok"}"#).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(
                read_agent_report(&path)
                    .unwrap_err()
                    .contains("regular file")
            );
        }
    }

    #[test]
    fn resume_cancels_agents_that_never_reported() {
        let root = tempfile::tempdir().unwrap();
        let (manager, project_id, task_id) = started_task(root.path());
        manager
            .record_agent_activity(
                project_id,
                task_id,
                "writer".to_owned(),
                "specification-generation".to_owned(),
                AgentStatus::Starting,
                None,
            )
            .unwrap();
        let unavailable = crate::integrations::ToolCommands::with_paths(
            "/nonexistent/pi".into(),
            "/nonexistent/herdr".into(),
            "/nonexistent/openspec".into(),
            "git".into(),
        );
        assert_eq!(
            super::cancel_interrupted_agents(&manager, &unavailable, project_id, task_id).unwrap(),
            1
        );
        let task = manager.get_task(project_id, task_id).unwrap();
        assert_eq!(
            task.agent_activity.last().unwrap().status,
            AgentStatus::Cancelled
        );
        assert!(
            manager
                .interrupted_agents(project_id, task_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn agent_contract_states_result_file_change_and_write_rules() {
        let root = tempfile::tempdir().unwrap();
        let (manager, project_id, task_id) = started_task(root.path());
        let task = manager.get_task(project_id, task_id).unwrap();
        let worktree = Path::new("/work/tree");
        let review = agent_contract(&task, WorkflowStep::CodeReview, "reviewer", worktree, 1);
        assert!(review.contains("/work/tree/.cronos/results/code-review/reviewer.json"));
        assert!(review.contains(&task.openspec_change()));
        assert!(review.contains("read-only"));
        assert!(review.contains("Never push"));
        let code = agent_contract(&task, WorkflowStep::CodeGeneration, "builder", worktree, 1);
        assert!(code.contains("commit with a Conventional Commit message"));
        let docs = agent_contract(&task, WorkflowStep::Documentation, "docs", worktree, 1);
        assert!(docs.contains("do not commit or archive"));
        assert!(code.contains("You are the only writer"));
        let part = agent_contract(&task, WorkflowStep::CodeGeneration, "api", worktree, 3);
        assert!(part.contains("one of 3 code-generation agents"));
        assert!(part.contains("change only the files your part needs"));
        assert!(!part.contains("You are the only writer"));
    }
}
