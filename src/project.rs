use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    integrations::{GitAdapter, GitError, IntegrationError, PullRequest, ToolCommands},
    model::{
        AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, ApprovalRecord,
        MAX_AGENT_EVENTS, MAX_APPROVAL_RECORDS, MAX_RECOVERY_RECORDS, ModelError, Priority,
        Project, ProjectId, ProjectStatus, PublishMode, RecoveryAction, RecoveryRecord,
        Requirement, RequirementId, RequirementKind, Task, TaskId, TaskStatus, WorkflowStep,
        normalize_text,
    },
    planning::{Plan, PlanError, parse_plan, planning_prompt},
    release::{Version, changelog_section, insert_section, next_version, utc_date},
    step_skills::install_planning_skill,
    storage::{ProjectStore, StoreError},
};

/// Automatic returns to code generation allowed per attempt before the task stops with Error.
pub const MAX_AUTOMATIC_REVISIONS: u32 = 3;

/// Result of a local-merge attempt.
#[derive(Debug)]
pub enum MergeOutcome {
    Completed(Task),
    /// The base repository has uncommitted changes; the task waits at local merge.
    BaseDirty(String),
    /// The merge conflicted and was aborted; the task returned to code generation.
    Conflict(Task),
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("cannot add tasks to an archived project")]
    ArchivedProject,
    #[error("dependency {0:?} does not belong to this project")]
    UnknownDependency(TaskId),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("invalid task status transition: {from:?} -> {to:?}")]
    InvalidStatusTransition { from: TaskStatus, to: TaskStatus },
    #[error("cannot change workflow step while task status is {0:?}")]
    TaskNotInProgress(TaskStatus),
    #[error("approval for {gate:?} is only valid at step {step}")]
    InvalidApprovalStage {
        gate: ApprovalGate,
        step: &'static str,
    },
    #[error("approval for {0:?} is required before advancing")]
    ApprovalRequired(ApprovalGate),
    #[error("a rejection must include a revision note")]
    ApprovalNoteRequired,
    #[error("abandoning a task requires a reason")]
    AbandonmentNoteRequired,
    #[error("completed status requires final approval and local merge")]
    CompletionNotReady,
    #[error("use complete_task to mark a task completed")]
    UseCompleteTask,
    #[error("use abandon_task to reject a task")]
    UseAbandonTask,
    #[error("Error tasks must be resumed through retry_task or clarify_task")]
    UseRecoveryAction,
    #[error("task recovery is only valid while status is Error, not {0:?}")]
    TaskNotInError(TaskStatus),
    #[error("clarifying a task requires nonblank context")]
    ClarificationRequired,
    #[error("task attempt counter overflowed")]
    AttemptOverflow,
    #[error("task is waiting for human approval")]
    AwaitingHumanApproval,
    #[error("expected workflow step {expected:?}, found {actual:?}")]
    UnexpectedWorkflowStep {
        expected: WorkflowStep,
        actual: Option<String>,
    },
    #[error("task has an unknown workflow step: {0}")]
    UnknownWorkflowStep(String),
    #[error("task worktree is missing or outside its assigned directory")]
    InvalidTaskWorktree,
    #[error("project has no Git repository configured")]
    RepositoryRequired,
    #[error("configured project repository changed; verify it before continuing")]
    RepositoryChanged,
    #[error("a project repository cannot change while tasks are running or have worktrees")]
    RepositoryInUse,
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("system clock is before the Unix epoch")]
    InvalidSystemTime,
    #[error(
        "the repository has no committed openspec/ directory; run `openspec init` and commit openspec/ before starting tasks"
    )]
    OpenSpecNotInitialized,
    #[error("automatic revision limit ({MAX_AUTOMATIC_REVISIONS}) reached for this attempt")]
    AutomaticRevisionLimit,
    #[error("a check command needs a program")]
    CheckCommandRequired,
    #[error(
        "a workflow step is still running for this task in another process; wait for it to finish"
    )]
    TaskBusy,
    #[error("an open task already has this title: {0}")]
    DuplicateTask(String),
    #[error("the project has no requirement {0:?}")]
    UnknownRequirement(RequirementId),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("the project has no plan waiting for approval")]
    NoPendingPlan,
    #[error("add requirements before planning tasks")]
    NoRequirements,
    #[error("the planner failed: {0}")]
    Planner(IntegrationError),
}

/// Exclusive per-task lock held while a workflow step runs. The operating system releases it when
/// the holder exits, so a crashed process never leaves the task locked.
#[derive(Debug)]
pub struct TaskLock {
    _file: fs::File,
}

pub struct ProjectManager {
    store: ProjectStore,
}

impl ProjectManager {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self {
            store: ProjectStore::new(root)?,
        })
    }

    pub fn data_dir(&self) -> &Path {
        self.store.root()
    }

    /// Takes the task's step lock, failing with `TaskBusy` when another process holds it.
    pub fn lock_task(&self, task_id: TaskId) -> Result<TaskLock, ProjectError> {
        let directory =
            crate::integrations::ensure_real_directory(&self.store.root().join("locks"))
                .map_err(StoreError::Io)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(format!("{}.lock", task_id.0)))
            .map_err(StoreError::Io)?;
        // A child forked by another thread shares the lock until it execs, so a lock released a
        // moment ago can look held. Real holders run a whole step; a short retry tells them apart.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(TaskLock { _file: file }),
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(fs::TryLockError::WouldBlock) => return Err(ProjectError::TaskBusy),
                Err(fs::TryLockError::Error(error)) => return Err(StoreError::Io(error).into()),
            }
        }
    }

    pub fn create_project(
        &self,
        name: String,
        description: Option<String>,
    ) -> Result<Project, ProjectError> {
        self.create_project_with_repository(
            name,
            description,
            None,
            None,
            None,
            &GitAdapter::default(),
        )
    }

    pub fn create_project_with_repository(
        &self,
        name: String,
        description: Option<String>,
        repository: Option<&Path>,
        check_command: Option<Vec<String>>,
        check_timeout_minutes: Option<u32>,
        git: &GitAdapter,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let repository = repository
            .map(|path| {
                git.repository_root(path)
                    .map(|root| root.to_string_lossy().into_owned())
            })
            .transpose()?;
        let now = now_unix_secs()?;
        let project = Project {
            id: ProjectId::new(),
            name,
            description,
            status: ProjectStatus::Active,
            task_ids: Vec::new(),
            repository,
            check_command,
            check_timeout_minutes,
            publish: Default::default(),
            remote: None,
            releases: false,
            requirements: Vec::new(),
            created_at_unix_secs: now,
            updated_at_unix_secs: now,
        };
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    pub fn set_project_repository(
        &self,
        project_id: ProjectId,
        repository: &Path,
        git: &GitAdapter,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        if self
            .store
            .list_tasks(project_id)?
            .iter()
            .any(|task| task.status == TaskStatus::InProgress || task.worktree.is_some())
        {
            return Err(ProjectError::RepositoryInUse);
        }
        project.repository = Some(
            git.repository_root(repository)?
                .to_string_lossy()
                .into_owned(),
        );
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    fn repository_path(
        &self,
        project: &Project,
        git: &GitAdapter,
    ) -> Result<std::path::PathBuf, ProjectError> {
        let configured = project
            .repository
            .as_deref()
            .ok_or(ProjectError::RepositoryRequired)?;
        let root = git.repository_root(Path::new(configured))?;
        if root.to_string_lossy() != configured {
            return Err(ProjectError::RepositoryChanged);
        }
        Ok(root)
    }

    pub fn set_project_check_command(
        &self,
        project_id: ProjectId,
        command: Vec<String>,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        if command.is_empty() {
            return Err(ProjectError::CheckCommandRequired);
        }
        let mut project = self.store.load_project(project_id)?;
        project.check_command = Some(command);
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    /// Sets how long the check command may run; `None` restores the default.
    pub fn set_project_check_timeout(
        &self,
        project_id: ProjectId,
        minutes: Option<u32>,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        project.check_timeout_minutes = minutes;
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    /// Turns release tags and changelog entries for locally merged tasks on or off.
    pub fn set_project_releases(
        &self,
        project_id: ProjectId,
        releases: bool,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        project.releases = releases;
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    /// Sets how completed tasks are published and which remote is used; `None` means `origin`.
    pub fn set_project_publish(
        &self,
        project_id: ProjectId,
        publish: PublishMode,
        remote: Option<String>,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        project.publish = publish;
        project.remote = remote;
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    pub fn get_project(&self, project_id: ProjectId) -> Result<Project, ProjectError> {
        Ok(self.store.load_project(project_id)?)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, ProjectError> {
        let mut projects = self.store.list_projects()?;
        projects.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        Ok(projects)
    }

    pub fn select_project(&self, id: ProjectId) -> Result<Project, ProjectError> {
        let project = self.store.load_project(id)?;
        if project.status == ProjectStatus::Archived {
            return Err(ProjectError::ArchivedProject);
        }
        Ok(project)
    }

    pub fn update_project(
        &self,
        id: ProjectId,
        name: Option<String>,
        description: Option<String>,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(id)?;
        if let Some(name) = name {
            project.name = name;
        }
        if let Some(description) = description {
            project.description = Some(description);
        }
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    pub fn archive_project(&self, id: ProjectId) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(id)?;
        project.status = ProjectStatus::Archived;
        project.updated_at_unix_secs = now_unix_secs()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    /// Adds a requirement with normalized text; the same requirement typed twice is rejected.
    pub fn add_requirement(
        &self,
        project_id: ProjectId,
        kind: RequirementKind,
        text: String,
    ) -> Result<Requirement, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        let now = now_unix_secs()?;
        let requirement = Requirement {
            id: RequirementId::new(),
            kind,
            text: normalize_text(&text),
            created_at_unix_secs: now,
        };
        project.requirements.push(requirement.clone());
        project.updated_at_unix_secs = now;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(requirement)
    }

    /// Removes a requirement. Tasks planned from it keep the link as history.
    pub fn remove_requirement(
        &self,
        project_id: ProjectId,
        requirement_id: RequirementId,
    ) -> Result<Project, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        let before = project.requirements.len();
        project
            .requirements
            .retain(|requirement| requirement.id != requirement_id);
        if project.requirements.len() == before {
            return Err(ProjectError::UnknownRequirement(requirement_id));
        }
        project.updated_at_unix_secs = now_unix_secs()?;
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    /// Runs the read-only planner on the project's repository and keeps its plan for approval.
    /// The store is not locked while the planner runs, so the project stays usable meanwhile.
    pub fn plan_tasks(
        &self,
        project_id: ProjectId,
        tools: &ToolCommands,
        timeout: std::time::Duration,
    ) -> Result<Plan, ProjectError> {
        let project = self.store.load_project(project_id)?;
        if project.requirements.is_empty() {
            return Err(ProjectError::NoRequirements);
        }
        let repository = self.repository_path(&project, &GitAdapter::with_tools(tools.clone()))?;
        let tasks = self.store.list_tasks(project_id)?;
        let skill = install_planning_skill(self.data_dir()).map_err(StoreError::Io)?;
        let answer = tools
            .run_planner(
                &repository,
                &skill,
                &planning_prompt(&project, &tasks),
                timeout,
            )
            .map_err(ProjectError::Planner)?;
        self.propose_plan(project_id, &answer)
    }

    /// Validates the planner's answer against the project and keeps it for human approval. An
    /// invalid answer is rejected and any earlier pending plan stays as it was.
    pub fn propose_plan(&self, project_id: ProjectId, answer: &str) -> Result<Plan, ProjectError> {
        let _write = self.store.lock_writes()?;
        let project = self.store.load_project(project_id)?;
        if project.requirements.is_empty() {
            return Err(ProjectError::NoRequirements);
        }
        let tasks = self.store.list_tasks(project_id)?;
        let mut plan = parse_plan(answer, &project, &tasks)?;
        plan.created_at_unix_secs = now_unix_secs()?;
        self.store.save_plan(project_id, &plan)?;
        Ok(plan)
    }

    pub fn pending_plan(&self, project_id: ProjectId) -> Result<Option<Plan>, ProjectError> {
        Ok(self.store.load_plan(project_id)?)
    }

    pub fn discard_plan(&self, project_id: ProjectId) -> Result<(), ProjectError> {
        let _write = self.store.lock_writes()?;
        Ok(self.store.delete_plan(project_id)?)
    }

    /// Creates the pending plan's tasks, dependencies first, and removes the plan. A planned task
    /// whose title matches an open task by then reuses that task; requirement links that were
    /// removed in the meantime are dropped.
    pub fn apply_plan(&self, project_id: ProjectId) -> Result<Vec<Task>, ProjectError> {
        let _write = self.store.lock_writes()?;
        let plan = self
            .store
            .load_plan(project_id)?
            .ok_or(ProjectError::NoPendingPlan)?;
        let project = self.store.load_project(project_id)?;
        let mut created: Vec<Option<Task>> = vec![None; plan.tasks.len()];
        for index in plan.creation_order() {
            let planned = &plan.tasks[index];
            let open = self.store.list_tasks(project_id)?.into_iter().find(|task| {
                !matches!(task.status, TaskStatus::Completed | TaskStatus::Rejected)
                    && task.title.to_lowercase() == planned.title.to_lowercase()
            });
            let task = match open {
                Some(task) => task,
                None => {
                    let dependencies = planned
                        .depends_on
                        .iter()
                        .filter_map(|dependency| created[*dependency].as_ref().map(|task| task.id))
                        .collect();
                    let requirement_ids = planned
                        .requirement_ids
                        .iter()
                        .copied()
                        .filter(|id| {
                            project
                                .requirements
                                .iter()
                                .any(|requirement| requirement.id == *id)
                        })
                        .collect();
                    self.create_task_for_requirements(
                        project_id,
                        planned.title.clone(),
                        planned.description.clone(),
                        planned.priority.clone(),
                        dependencies,
                        requirement_ids,
                    )?
                }
            };
            created[index] = Some(task);
        }
        self.store.delete_plan(project_id)?;
        Ok(created.into_iter().flatten().collect())
    }

    pub fn create_task(
        &self,
        project_id: ProjectId,
        title: String,
        description: Option<String>,
        priority: Priority,
        dependencies: Vec<TaskId>,
    ) -> Result<Task, ProjectError> {
        self.create_task_for_requirements(
            project_id,
            title,
            description,
            priority,
            dependencies,
            Vec::new(),
        )
    }

    /// Creates a task with a normalized title, rejecting one that duplicates an open task (any
    /// task that is not Completed or Rejected), and links it to the requirements it fulfils.
    pub fn create_task_for_requirements(
        &self,
        project_id: ProjectId,
        title: String,
        description: Option<String>,
        priority: Priority,
        dependencies: Vec<TaskId>,
        requirement_ids: Vec<RequirementId>,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut project = self.store.load_project(project_id)?;
        let title = normalize_text(&title);
        let key = title.to_lowercase();
        if self.store.list_tasks(project_id)?.iter().any(|task| {
            !matches!(task.status, TaskStatus::Completed | TaskStatus::Rejected)
                && task.title.to_lowercase() == key
        }) {
            return Err(ProjectError::DuplicateTask(title));
        }
        if let Some(unknown) = requirement_ids.iter().find(|id| {
            !project
                .requirements
                .iter()
                .any(|requirement| requirement.id == **id)
        }) {
            return Err(ProjectError::UnknownRequirement(*unknown));
        }
        if project.status == ProjectStatus::Archived {
            return Err(ProjectError::ArchivedProject);
        }

        let known_tasks: HashSet<_> = project.task_ids.iter().copied().collect();
        for dependency in &dependencies {
            if !known_tasks.contains(dependency) {
                return Err(ProjectError::UnknownDependency(*dependency));
            }
            self.store.load_task(project_id, *dependency)?;
        }

        let now = now_unix_secs()?;
        let task = Task {
            id: TaskId::new(),
            project_id,
            title,
            description,
            priority,
            dependencies,
            status: TaskStatus::Pending,
            current_step: None,
            step_started_at_unix_secs: Default::default(),
            agent_activity: Vec::new(),
            approval_history: Vec::new(),
            abandonment_note: None,
            worktree: None,
            attempt: 1,
            recovery_history: Vec::new(),
            automatic_revisions: 0,
            published: None,
            release: None,
            requirement_ids,
            created_at_unix_secs: now,
            updated_at_unix_secs: now,
        };
        task.validate()?;

        self.store.save_task(&task)?;
        project.task_ids.push(task.id);
        project.updated_at_unix_secs = now;
        self.store.save_project(&project)?;
        Ok(task)
    }

    pub fn list_tasks(&self, project_id: ProjectId) -> Result<Vec<Task>, ProjectError> {
        Ok(self.store.list_tasks(project_id)?)
    }

    pub fn get_task(&self, project_id: ProjectId, task_id: TaskId) -> Result<Task, ProjectError> {
        Ok(self.store.load_task(project_id, task_id)?)
    }

    pub fn prepare_task_worktree(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        git: &GitAdapter,
    ) -> Result<std::path::PathBuf, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status == TaskStatus::Completed || task.status == TaskStatus::Rejected {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        if task.worktree.is_some() {
            return self.validated_task_worktree_path(project_id, task_id);
        }
        let project = self.store.load_project(project_id)?;
        let repository = self.repository_path(&project, git)?;
        let worktree = self
            .store
            .expected_task_worktree_path(project_id, task_id, task.attempt);
        let worktrees_root = self.store.worktrees_root();
        fs::create_dir_all(&worktrees_root).map_err(StoreError::Io)?;
        if fs::canonicalize(&worktrees_root).map_err(StoreError::Io)? != worktrees_root {
            return Err(ProjectError::InvalidTaskWorktree);
        }
        let parent = worktree.parent().ok_or(ProjectError::InvalidTaskWorktree)?;
        fs::create_dir_all(parent).map_err(StoreError::Io)?;
        if fs::canonicalize(parent).map_err(StoreError::Io)? != parent
            || !parent.starts_with(&worktrees_root)
        {
            return Err(ProjectError::InvalidTaskWorktree);
        }
        let branch = GitAdapter::worktree_branch(task.id, task.attempt);
        git.ensure_local_exclude(&repository, crate::workflow::CRONOS_EXCLUDE_PATTERN)?;
        git.add_worktree(&repository, &worktree, &branch)?;
        // Worktrees contain only committed files, so this also rejects an uncommitted openspec/.
        if !worktree.join("openspec").is_dir() {
            let _ = git.discard_attempt(&repository, &worktree, &branch);
            return Err(ProjectError::OpenSpecNotInitialized);
        }
        task.worktree = Some(worktree.to_string_lossy().into_owned());
        task.updated_at_unix_secs = now_unix_secs()?;
        if let Err(error) = self.store.save_task(&task) {
            let _ = git.discard_attempt(&repository, &worktree, &branch);
            return Err(error.into());
        }
        Ok(worktree)
    }

    pub fn validated_task_worktree_path(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
    ) -> Result<std::path::PathBuf, ProjectError> {
        let task = self.store.load_task(project_id, task_id)?;
        let expected = self
            .store
            .expected_task_worktree_path(project_id, task_id, task.attempt);
        if task.worktree.as_deref() != expected.to_str() {
            return Err(ProjectError::InvalidTaskWorktree);
        }
        let root = self.store.worktrees_root();
        let canonical_root = fs::canonicalize(&root).map_err(StoreError::Io)?;
        let canonical_task = fs::canonicalize(&expected).map_err(StoreError::Io)?;
        if canonical_root != root
            || canonical_task != expected
            || !canonical_task.starts_with(&canonical_root)
            || !canonical_task.is_dir()
        {
            return Err(ProjectError::InvalidTaskWorktree);
        }
        Ok(canonical_task)
    }

    pub fn set_task_status(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        status: TaskStatus,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status == status {
            return Ok(task);
        }
        if status == TaskStatus::Completed {
            return Err(ProjectError::UseCompleteTask);
        }
        if status == TaskStatus::Rejected {
            return Err(ProjectError::UseAbandonTask);
        }
        if task.status == TaskStatus::Error && status == TaskStatus::InProgress {
            return Err(ProjectError::UseRecoveryAction);
        }
        if !valid_status_transition(&task.status, &status) {
            return Err(ProjectError::InvalidStatusTransition {
                from: task.status,
                to: status,
            });
        }
        task.status = status;
        task.updated_at_unix_secs = now_unix_secs()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    fn set_task_step(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        step: String,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        if matches!(
            step.as_str(),
            "code-generation" | "code-review" | "verification" | "final-approval"
        ) && !task.is_approved(ApprovalGate::Specifications)
        {
            return Err(ProjectError::ApprovalRequired(ApprovalGate::Specifications));
        }
        if matches!(step.as_str(), "documentation" | "local-merge")
            && !task.is_approved(ApprovalGate::FinalCode)
        {
            return Err(ProjectError::ApprovalRequired(ApprovalGate::FinalCode));
        }
        let now = now_unix_secs()?;
        task.current_step = Some(step.clone());
        task.step_started_at_unix_secs.insert(step, now);
        task.updated_at_unix_secs = now;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn record_approval(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        gate: ApprovalGate,
        decision: ApprovalDecision,
        note: Option<String>,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        if task.current_step.as_deref() != Some(gate.approval_step()) {
            return Err(ProjectError::InvalidApprovalStage {
                gate,
                step: gate.approval_step(),
            });
        }
        let note = note
            .map(|note| note.trim().to_owned())
            .filter(|note| !note.is_empty());
        if decision == ApprovalDecision::Rejected && note.is_none() {
            return Err(ProjectError::ApprovalNoteRequired);
        }

        let now = now_unix_secs()?;
        if gate == ApprovalGate::Specifications {
            task.approval_history
                .retain(|record| record.gate != ApprovalGate::FinalCode);
        }
        task.approval_history.push(ApprovalRecord {
            attempt: task.attempt,
            gate,
            decision,
            note,
            decided_at_unix_secs: now,
        });
        if task.approval_history.len() > MAX_APPROVAL_RECORDS {
            task.approval_history.remove(0);
        }
        let next_step = match decision {
            ApprovalDecision::Approved => match gate {
                ApprovalGate::Specifications => "code-generation",
                ApprovalGate::FinalCode => "documentation",
            },
            ApprovalDecision::Rejected => gate.revision_step(),
        };
        task.current_step = Some(next_step.to_owned());
        task.step_started_at_unix_secs
            .insert(next_step.to_owned(), now);
        task.updated_at_unix_secs = now;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn abandon_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        reason: String,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(ProjectError::AbandonmentNoteRequired);
        }
        let mut task = self.store.load_task(project_id, task_id)?;
        if !valid_status_transition(&task.status, &TaskStatus::Rejected) {
            return Err(ProjectError::InvalidStatusTransition {
                from: task.status,
                to: TaskStatus::Rejected,
            });
        }
        task.status = TaskStatus::Rejected;
        task.abandonment_note = Some(reason);
        task.updated_at_unix_secs = now_unix_secs()?;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn start_next_task(&self, project_id: ProjectId) -> Result<Option<Task>, ProjectError> {
        let _write = self.store.lock_writes()?;
        if self
            .list_tasks(project_id)?
            .iter()
            .any(|task| matches!(task.status, TaskStatus::InProgress | TaskStatus::Error))
        {
            return Ok(None);
        }
        let Some(task) = self.ready_tasks(project_id)?.into_iter().next() else {
            return Ok(None);
        };
        self.set_task_status(project_id, task.id, TaskStatus::InProgress)?;
        self.set_task_step(
            project_id,
            task.id,
            WorkflowStep::SpecificationGeneration.as_str().to_owned(),
        )
        .map(Some)
    }

    pub fn complete_workflow_step(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        step: WorkflowStep,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        let Some(current_step) = task.current_step.as_deref() else {
            return Err(ProjectError::UnexpectedWorkflowStep {
                expected: step,
                actual: None,
            });
        };
        if current_step != step.as_str() {
            return Err(ProjectError::UnexpectedWorkflowStep {
                expected: step,
                actual: Some(current_step.to_owned()),
            });
        }
        if step.requires_human_approval() {
            return Err(ProjectError::AwaitingHumanApproval);
        }
        match step.next() {
            Some(next) => self.set_task_step(project_id, task_id, next.as_str().to_owned()),
            None => self.complete_task(project_id, task_id),
        }
    }

    /// Finishes the local-merge step the way the project publishes: merge locally, merge and push
    /// the base branch, or push the task branch and open a pull request. Every stage is idempotent,
    /// so after a failed publish a step retry picks up where it stopped.
    pub fn merge_and_complete_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        git: &GitAdapter,
    ) -> Result<MergeOutcome, ProjectError> {
        let _write = self.store.lock_writes()?;
        let task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress
            || task.current_step.as_deref() != Some(WorkflowStep::LocalMerge.as_str())
            || !task.is_approved(ApprovalGate::FinalCode)
        {
            return Err(ProjectError::AwaitingHumanApproval);
        }
        let project = self.store.load_project(project_id)?;
        let repository = self.repository_path(&project, git)?;
        let worktree = self.validated_task_worktree_path(project_id, task_id)?;
        let branch = GitAdapter::worktree_branch(task.id, task.attempt);
        let remote = project.remote_name();
        // Pull requests are merged elsewhere, so only local merges are released here.
        let mut release = None;
        let published = match project.publish {
            PublishMode::PullRequest => {
                let base = git.current_branch(&repository)?;
                let request = PullRequest {
                    remote,
                    base: &base,
                    head: &branch,
                    title: &task.title,
                    body: &pull_request_body(&task),
                };
                match git.open_pull_request(&repository, &worktree, &request) {
                    Ok(url) => Some(url),
                    Err(error) => return Err(self.publishing_failed(project_id, task_id, error)?),
                }
            }
            PublishMode::Local | PublishMode::Push => {
                if !git.is_merged(&repository, &branch)? {
                    match git.merge_worktree(&repository, &worktree, &branch) {
                        Ok(()) => {}
                        Err(GitError::DirtyBase(path)) => return Ok(MergeOutcome::BaseDirty(path)),
                        Err(GitError::MergeConflict(detail)) => {
                            return self.merge_conflicted(
                                project_id,
                                task_id,
                                git,
                                &repository,
                                detail,
                            );
                        }
                        Err(GitError::MergeFailed(detail)) => {
                            self.record_agent_activity(
                                project_id,
                                task_id,
                                "cronos".to_owned(),
                                WorkflowStep::LocalMerge.as_str().to_owned(),
                                AgentStatus::Failed,
                                Some(
                                    format!("Local merge failed and needs a human: {detail}")
                                        .chars()
                                        .take(2_000)
                                        .collect(),
                                ),
                            )?;
                            self.set_task_status(project_id, task_id, TaskStatus::Error)?;
                            return Err(GitError::MergeFailed(detail).into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                if project.releases {
                    match release_merged_task(git, &repository, &task) {
                        Ok(version) => release = Some(version),
                        Err(error) => {
                            return Err(self.publishing_failed(project_id, task_id, error)?);
                        }
                    }
                }
                if project.publish == PublishMode::Push {
                    let base = git.current_branch(&repository)?;
                    let pushed = git.push(&repository, remote, &base).and_then(|()| {
                        release.map_or(Ok(()), |version| {
                            git.push_tag(&repository, remote, &version.to_string())
                        })
                    });
                    match pushed {
                        Ok(()) => Some(format!("pushed {base} to {remote}")),
                        Err(error) => {
                            return Err(self.publishing_failed(project_id, task_id, error)?);
                        }
                    }
                } else {
                    None
                }
            }
        };
        if published.is_some() || release.is_some() {
            let mut task = self.store.load_task(project_id, task_id)?;
            task.published = published;
            task.release = release.map(|version| version.to_string());
            task.updated_at_unix_secs = now_unix_secs()?;
            task.validate()?;
            self.store.save_task(&task)?;
        }
        Ok(MergeOutcome::Completed(self.complete_workflow_step(
            project_id,
            task_id,
            WorkflowStep::LocalMerge,
        )?))
    }

    fn merge_conflicted(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        git: &GitAdapter,
        repository: &Path,
        detail: String,
    ) -> Result<MergeOutcome, ProjectError> {
        let branch = git.current_branch(repository)?;
        self.record_agent_activity(
            project_id,
            task_id,
            "cronos".to_owned(),
            WorkflowStep::LocalMerge.as_str().to_owned(),
            AgentStatus::ChangesRequested,
            Some(
                format!(
                    "Local merge into {branch} conflicted and was aborted. Merge {branch} into the task branch, resolve the conflicts, and commit. {detail}"
                )
                .chars()
                .take(2_000)
                .collect(),
            ),
        )?;
        Ok(MergeOutcome::Conflict(
            self.request_automatic_revision(project_id, task_id)?,
        ))
    }

    /// Records a failed push or pull request and stops the task with Error; a step retry resumes
    /// publishing without merging again.
    fn publishing_failed(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        error: GitError,
    ) -> Result<ProjectError, ProjectError> {
        self.record_agent_activity(
            project_id,
            task_id,
            "cronos".to_owned(),
            WorkflowStep::LocalMerge.as_str().to_owned(),
            AgentStatus::Failed,
            Some(
                format!("Publishing failed; fix the cause, then retry the step: {error}")
                    .chars()
                    .take(2_000)
                    .collect(),
            ),
        )?;
        self.set_task_status(project_id, task_id, TaskStatus::Error)?;
        Ok(error.into())
    }

    /// Returns an in-progress task to code generation after review, verification, or merge findings.
    /// Final-code approval of the attempt is cleared because the code will change.
    pub fn request_automatic_revision(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        let now = now_unix_secs()?;
        task.updated_at_unix_secs = now;
        if task.automatic_revisions >= MAX_AUTOMATIC_REVISIONS {
            task.agent_activity.push(AgentActivity {
                attempt: task.attempt,
                agent_name: "cronos".to_owned(),
                step: task.current_step.clone().unwrap_or_default(),
                status: AgentStatus::Failed,
                detail: Some(format!(
                    "Automatic revision limit ({MAX_AUTOMATIC_REVISIONS}) reached for attempt {}; review the findings, then retry the step or the task.",
                    task.attempt
                )),
                updated_at_unix_secs: now,
            });
            if task.agent_activity.len() > MAX_AGENT_EVENTS {
                task.agent_activity.remove(0);
            }
            task.status = TaskStatus::Error;
            task.validate()?;
            self.store.save_task(&task)?;
            return Err(ProjectError::AutomaticRevisionLimit);
        }
        task.automatic_revisions += 1;
        let attempt = task.attempt;
        task.approval_history.retain(|record| {
            !(record.gate == ApprovalGate::FinalCode && record.attempt == attempt)
        });
        let step = WorkflowStep::CodeGeneration.as_str().to_owned();
        task.current_step = Some(step.clone());
        task.step_started_at_unix_secs.insert(step, now);
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    /// Resumes an Error task at its failed step, keeping the attempt, worktree, and approvals.
    pub fn retry_step(&self, project_id: ProjectId, task_id: TaskId) -> Result<Task, ProjectError> {
        let _lock = self.lock_task(task_id)?;
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::Error {
            return Err(ProjectError::TaskNotInError(task.status));
        }
        let step = task
            .current_step
            .clone()
            .ok_or(ProjectError::UnexpectedWorkflowStep {
                expected: WorkflowStep::SpecificationGeneration,
                actual: None,
            })?;
        if WorkflowStep::parse(&step).is_none() {
            return Err(ProjectError::UnknownWorkflowStep(step));
        }
        if task.worktree.is_some() {
            self.validated_task_worktree_path(project_id, task_id)?;
        }
        let now = now_unix_secs()?;
        task.status = TaskStatus::InProgress;
        task.step_started_at_unix_secs.insert(step, now);
        task.recovery_history.push(RecoveryRecord {
            attempt: task.attempt,
            action: RecoveryAction::RetryStep,
            note: None,
            discarded_worktree: None,
            resumed_at_unix_secs: now,
        });
        if task.recovery_history.len() > MAX_RECOVERY_RECORDS {
            task.recovery_history.remove(0);
        }
        task.updated_at_unix_secs = now;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    /// Agents of the current attempt and step that started but never reported an outcome.
    pub fn interrupted_agents(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
    ) -> Result<Vec<String>, ProjectError> {
        let task = self.store.load_task(project_id, task_id)?;
        let Some(step) = task.current_step.as_deref() else {
            return Ok(Vec::new());
        };
        let mut last = std::collections::BTreeMap::new();
        for activity in task
            .agent_activity
            .iter()
            .filter(|activity| activity.attempt == task.attempt && activity.step == step)
        {
            last.insert(activity.agent_name.clone(), activity.status.clone());
        }
        Ok(last
            .into_iter()
            .filter(|(_, status)| matches!(status, AgentStatus::Starting | AgentStatus::Running))
            .map(|(name, _)| name)
            .collect())
    }

    pub fn retry_task(&self, project_id: ProjectId, task_id: TaskId) -> Result<Task, ProjectError> {
        self.recover_task(project_id, task_id, RecoveryAction::Retry, None, None)
    }

    pub fn retry_task_with_git(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        git: &GitAdapter,
    ) -> Result<Task, ProjectError> {
        self.recover_task(project_id, task_id, RecoveryAction::Retry, None, Some(git))
    }

    pub fn clarify_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        context: String,
    ) -> Result<Task, ProjectError> {
        let context = context.trim().to_owned();
        if context.is_empty() {
            return Err(ProjectError::ClarificationRequired);
        }
        self.recover_task(
            project_id,
            task_id,
            RecoveryAction::Clarify,
            Some(context),
            None,
        )
    }

    pub fn clarify_task_with_git(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        context: String,
        git: &GitAdapter,
    ) -> Result<Task, ProjectError> {
        let context = context.trim().to_owned();
        if context.is_empty() {
            return Err(ProjectError::ClarificationRequired);
        }
        self.recover_task(
            project_id,
            task_id,
            RecoveryAction::Clarify,
            Some(context),
            Some(git),
        )
    }

    fn recover_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        action: RecoveryAction,
        note: Option<String>,
        git: Option<&GitAdapter>,
    ) -> Result<Task, ProjectError> {
        // Never discard a worktree while a step process may still be working in it.
        let _lock = self.lock_task(task_id)?;
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::Error {
            return Err(ProjectError::TaskNotInError(task.status));
        }
        let attempt = task
            .attempt
            .checked_add(1)
            .ok_or(ProjectError::AttemptOverflow)?;
        let discarded_worktree = if task.worktree.is_some() {
            let path = self.validated_task_worktree_path(project_id, task_id)?;
            if let Some(git) = git {
                let project = self.store.load_project(project_id)?;
                let repository = self.repository_path(&project, git)?;
                git.discard_attempt(
                    &repository,
                    &path,
                    &GitAdapter::worktree_branch(task.id, task.attempt),
                )?;
            }
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        let now = now_unix_secs()?;
        task.attempt = attempt;
        task.status = TaskStatus::InProgress;
        task.current_step = Some(WorkflowStep::SpecificationGeneration.as_str().to_owned());
        task.step_started_at_unix_secs.insert(
            WorkflowStep::SpecificationGeneration.as_str().to_owned(),
            now,
        );
        task.worktree = None;
        task.automatic_revisions = 0;
        task.recovery_history.push(RecoveryRecord {
            attempt,
            action,
            note,
            discarded_worktree,
            resumed_at_unix_secs: now,
        });
        if task.recovery_history.len() > MAX_RECOVERY_RECORDS {
            task.recovery_history.remove(0);
        }
        task.updated_at_unix_secs = now;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn complete_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        if !task.is_approved(ApprovalGate::FinalCode)
            || task.current_step.as_deref() != Some("local-merge")
        {
            return Err(ProjectError::CompletionNotReady);
        }
        task.status = TaskStatus::Completed;
        task.updated_at_unix_secs = now_unix_secs()?;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn record_agent_activity(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        agent_name: String,
        step: String,
        status: AgentStatus,
        detail: Option<String>,
    ) -> Result<Task, ProjectError> {
        let _write = self.store.lock_writes()?;
        let mut task = self.store.load_task(project_id, task_id)?;
        if task.status != TaskStatus::InProgress {
            return Err(ProjectError::TaskNotInProgress(task.status));
        }
        let now = now_unix_secs()?;
        task.agent_activity.push(AgentActivity {
            attempt: task.attempt,
            agent_name,
            step,
            status,
            detail,
            updated_at_unix_secs: now,
        });
        if task.agent_activity.len() > MAX_AGENT_EVENTS {
            task.agent_activity.remove(0);
        }
        task.updated_at_unix_secs = now;
        task.validate()?;
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn ready_tasks(&self, project_id: ProjectId) -> Result<Vec<Task>, ProjectError> {
        let tasks = self.list_tasks(project_id)?;
        let completed: HashSet<_> = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .map(|task| task.id)
            .collect();
        let mut ready: Vec<_> = tasks
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::Pending
                    && task.dependencies.iter().all(|id| completed.contains(id))
            })
            .collect();
        ready.sort_by(|left, right| {
            priority_rank(&right.priority)
                .cmp(&priority_rank(&left.priority))
                .then_with(|| left.created_at_unix_secs.cmp(&right.created_at_unix_secs))
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        Ok(ready)
    }
}

/// Tags the merge just made as the next release and records it in `CHANGELOG.md`. A release
/// already tagged on HEAD is adopted, so retrying after a failed push never makes a second one.
fn release_merged_task(
    git: &GitAdapter,
    repository: &Path,
    task: &Task,
) -> Result<Version, GitError> {
    if let Some(version) = git.release_at_head(repository)? {
        return Ok(version);
    }
    let latest = git.latest_release(repository)?;
    // The first release covers only this task's merge rather than the whole history.
    let range = latest.map_or_else(
        || "HEAD^1..HEAD".to_owned(),
        |version| format!("{version}..HEAD"),
    );
    let commits = git.commits(repository, &range)?;
    let version = next_version(latest, &commits);
    let date =
        utc_date(now_unix_secs().map_err(|error| GitError::ReleaseFailed(error.to_string()))?);
    let existing = fs::read_to_string(repository.join(CHANGELOG)).ok();
    let content = insert_section(
        existing.as_deref(),
        &changelog_section(version, &date, &commits),
    );
    git.commit_release(
        repository,
        CHANGELOG,
        &content,
        &version,
        &format!("Release {version}: {}", task.title),
    )?;
    Ok(version)
}

const CHANGELOG: &str = "CHANGELOG.md";

fn pull_request_body(task: &Task) -> String {
    let description = task
        .description
        .as_deref()
        .map(|description| format!("{description}\n\n"))
        .unwrap_or_default();
    format!(
        "{description}Implements OpenSpec change `{}`.\n\nOpened by Cronos-AI for task {} (attempt {}) after final-code approval.",
        task.openspec_change(),
        task.id.0,
        task.attempt
    )
}

fn valid_status_transition(from: &TaskStatus, to: &TaskStatus) -> bool {
    matches!(
        (from, to),
        (
            TaskStatus::Pending,
            TaskStatus::InProgress | TaskStatus::Error | TaskStatus::Rejected
        ) | (
            TaskStatus::InProgress,
            TaskStatus::Error | TaskStatus::Completed | TaskStatus::Rejected
        ) | (
            TaskStatus::Error,
            TaskStatus::InProgress | TaskStatus::Rejected
        )
    )
}

fn priority_rank(priority: &Priority) -> u8 {
    match priority {
        Priority::Low => 0,
        Priority::Medium => 1,
        Priority::High => 2,
    }
}

fn now_unix_secs() -> Result<u64, ProjectError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ProjectError::InvalidSystemTime)
}

#[cfg(test)]
mod tests {
    use super::{MergeOutcome, ProjectError, ProjectManager};
    use crate::integrations::{GitAdapter, GitError};
    use crate::model::{
        AgentStatus, ApprovalDecision, ApprovalGate, MAX_AGENT_EVENTS, ModelError, Priority,
        ProjectId, ProjectStatus, PublishMode, RecoveryAction, RequirementKind, TaskId, TaskStatus,
        WorkflowStep,
    };
    use std::{fs, path::Path, process::Command};

    const EMPTY_GLOBAL_GIT_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

    fn git_fixture(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", EMPTY_GLOBAL_GIT_CONFIG)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_fixture(root: &Path) {
        fs::create_dir_all(root).unwrap();
        git_fixture(root, &["init"]);
        git_fixture(root, &["config", "user.name", "Cronos Tests"]);
        git_fixture(root, &["config", "user.email", "cronos@example.invalid"]);
        fs::write(root.join("README.md"), "base\n").unwrap();
        fs::create_dir_all(root.join("openspec")).unwrap();
        fs::write(root.join("openspec/config.yaml"), "schema: spec-driven\n").unwrap();
        git_fixture(root, &["add", "README.md", "openspec"]);
        git_fixture(root, &["commit", "-m", "initial"]);
    }

    fn complete_to_local_merge(
        manager: &ProjectManager,
        project_id: ProjectId,
        task_id: crate::model::TaskId,
    ) {
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
        for step in [
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
        ] {
            manager
                .complete_workflow_step(project_id, task_id, step)
                .unwrap();
        }
        manager
            .record_approval(
                project_id,
                task_id,
                ApprovalGate::FinalCode,
                ApprovalDecision::Approved,
                None,
            )
            .unwrap();
        manager
            .complete_workflow_step(project_id, task_id, WorkflowStep::Documentation)
            .unwrap();
    }

    fn complete_for_queue(
        manager: &ProjectManager,
        project_id: ProjectId,
        task_id: crate::model::TaskId,
    ) {
        if manager.get_task(project_id, task_id).unwrap().status == TaskStatus::Pending {
            let started = manager.start_next_task(project_id).unwrap().unwrap();
            assert_eq!(started.id, task_id);
        }
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
        for step in [
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
        ] {
            manager
                .complete_workflow_step(project_id, task_id, step)
                .unwrap();
        }
        manager
            .record_approval(
                project_id,
                task_id,
                ApprovalGate::FinalCode,
                ApprovalDecision::Approved,
                None,
            )
            .unwrap();
        manager
            .complete_workflow_step(project_id, task_id, WorkflowStep::Documentation)
            .unwrap();
        manager
            .complete_workflow_step(project_id, task_id, WorkflowStep::LocalMerge)
            .unwrap();
    }

    #[test]
    fn requirements_are_normalized_deduplicated_and_removed() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(temp.path()).unwrap();
        let project = manager.create_project("Planned".to_owned(), None).unwrap();

        let export = manager
            .add_requirement(
                project.id,
                RequirementKind::Functional,
                "  Users can   export CSV ".to_owned(),
            )
            .unwrap();
        assert_eq!(export.text, "Users can export CSV");
        assert!(matches!(
            manager.add_requirement(
                project.id,
                RequirementKind::NonFunctional,
                "users can export csv".to_owned()
            ),
            Err(ProjectError::Model(ModelError::DuplicateRequirement))
        ));
        manager
            .add_requirement(
                project.id,
                RequirementKind::NonFunctional,
                "Exports finish within 2 seconds".to_owned(),
            )
            .unwrap();
        assert_eq!(
            manager.get_project(project.id).unwrap().requirements.len(),
            2
        );

        manager.remove_requirement(project.id, export.id).unwrap();
        let remaining = manager.get_project(project.id).unwrap().requirements;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].kind, RequirementKind::NonFunctional);
        assert!(matches!(
            manager.remove_requirement(project.id, export.id),
            Err(ProjectError::UnknownRequirement(_))
        ));
    }

    #[test]
    fn task_titles_are_normalized_and_open_duplicates_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(temp.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();

        let task = manager
            .create_task(
                project.id,
                "  Add   export ".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(task.title, "Add export");
        assert!(matches!(
            manager.create_task(
                project.id,
                "add EXPORT".to_owned(),
                None,
                Priority::Low,
                Vec::new()
            ),
            Err(ProjectError::DuplicateTask(_))
        ));

        // Abandoned work does not block doing the same thing again.
        manager
            .abandon_task(project.id, task.id, "Rethinking it".to_owned())
            .unwrap();
        manager
            .create_task(
                project.id,
                "Add export".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
    }

    const PLAN_ANSWER: &str = r#"```json
{"tasks": [
  {"key": "csv", "title": "Add CSV export", "priority": "high", "depends_on": [], "requirements": ["R1"]},
  {"key": "fast", "title": "Stream large exports", "description": "Keep memory flat.", "priority": "medium", "depends_on": ["csv"], "requirements": ["R2"]}
]}
```"#;

    fn planned_project(manager: &ProjectManager) -> (ProjectId, Vec<crate::model::RequirementId>) {
        let project = manager.create_project("Planned".to_owned(), None).unwrap();
        let ids = [
            ("Users can export CSV", RequirementKind::Functional),
            (
                "Exports stream in constant memory",
                RequirementKind::NonFunctional,
            ),
        ]
        .into_iter()
        .map(|(text, kind)| {
            manager
                .add_requirement(project.id, kind, text.to_owned())
                .unwrap()
                .id
        })
        .collect();
        (project.id, ids)
    }

    #[test]
    fn an_applied_plan_creates_linked_tasks_in_dependency_order() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(temp.path()).unwrap();
        let (project_id, requirements) = planned_project(&manager);

        let plan = manager.propose_plan(project_id, PLAN_ANSWER).unwrap();
        assert_eq!(plan.tasks.len(), 2);
        assert!(plan.created_at_unix_secs > 0);
        assert_eq!(manager.pending_plan(project_id).unwrap(), Some(plan));
        assert!(
            manager.list_tasks(project_id).unwrap().is_empty(),
            "nothing before approval"
        );

        let tasks = manager.apply_plan(project_id).unwrap();
        assert_eq!(tasks.len(), 2);
        let csv = tasks
            .iter()
            .find(|task| task.title == "Add CSV export")
            .unwrap();
        let fast = tasks
            .iter()
            .find(|task| task.title == "Stream large exports")
            .unwrap();
        assert_eq!(csv.priority, Priority::High);
        assert_eq!(csv.requirement_ids, vec![requirements[0]]);
        assert_eq!(fast.dependencies, vec![csv.id]);
        assert_eq!(fast.description.as_deref(), Some("Keep memory flat."));
        assert_eq!(manager.pending_plan(project_id).unwrap(), None);
        assert!(matches!(
            manager.apply_plan(project_id),
            Err(ProjectError::NoPendingPlan)
        ));
    }

    #[test]
    fn a_plan_reuses_open_tasks_and_invalid_answers_are_not_kept() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(temp.path()).unwrap();
        let (project_id, _) = planned_project(&manager);
        let existing = manager
            .create_task(
                project_id,
                "add csv export".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(
            manager.propose_plan(project_id, "I could not decide."),
            Err(ProjectError::Plan(crate::planning::PlanError::NoPlan))
        ));
        assert_eq!(manager.pending_plan(project_id).unwrap(), None);

        manager.propose_plan(project_id, PLAN_ANSWER).unwrap();
        let tasks = manager.apply_plan(project_id).unwrap();
        assert_eq!(manager.list_tasks(project_id).unwrap().len(), 2);
        let fast = tasks
            .iter()
            .find(|task| task.title == "Stream large exports")
            .unwrap();
        assert_eq!(fast.dependencies, vec![existing.id]);

        manager.propose_plan(project_id, PLAN_ANSWER).unwrap();
        manager.discard_plan(project_id).unwrap();
        assert_eq!(manager.pending_plan(project_id).unwrap(), None);
    }

    #[test]
    fn check_timeout_is_set_validated_and_reset() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(temp.path()).unwrap();
        let project = manager.create_project("Timed".to_owned(), None).unwrap();
        assert_eq!(project.check_timeout_minutes, None);

        let configured = manager
            .set_project_check_timeout(project.id, Some(90))
            .unwrap();
        assert_eq!(configured.check_timeout_minutes, Some(90));
        assert!(matches!(
            manager.set_project_check_timeout(project.id, Some(0)),
            Err(ProjectError::Model(ModelError::InvalidCheckTimeout))
        ));
        assert_eq!(
            manager
                .get_project(project.id)
                .unwrap()
                .check_timeout_minutes,
            Some(90)
        );

        let reset = manager.set_project_check_timeout(project.id, None).unwrap();
        assert_eq!(reset.check_timeout_minutes, None);
        assert_eq!(
            manager
                .get_project(project.id)
                .unwrap()
                .check_timeout_minutes,
            None
        );
    }

    #[test]
    fn pending_legacy_project_can_be_configured_with_a_git_repository() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        init_git_fixture(&repository);
        let manager = ProjectManager::new(temp.path().join("data")).unwrap();
        let project = manager.create_project("Legacy".to_owned(), None).unwrap();
        manager
            .create_task(
                project.id,
                "Pending".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();

        let configured = manager
            .set_project_repository(project.id, &repository, &GitAdapter::default())
            .unwrap();
        let expected_root = GitAdapter::default().repository_root(&repository).unwrap();
        assert_eq!(
            configured.repository.as_deref(),
            Some(expected_root.to_str().unwrap())
        );
    }

    #[test]
    fn repository_configuration_creates_attempt_worktrees_and_retry_removes_old_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let data = temp.path().join("data");
        init_git_fixture(&repository);
        let manager = ProjectManager::new(&data).unwrap();
        let git = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Factory".to_owned(),
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
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let old_worktree = manager
            .prepare_task_worktree(project.id, task.id, &git)
            .unwrap();
        fs::write(old_worktree.join("discarded.txt"), "old output").unwrap();
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();

        let retried = manager
            .retry_task_with_git(project.id, task.id, &git)
            .unwrap();
        assert_eq!(retried.attempt, 2);
        assert_eq!(retried.status, TaskStatus::InProgress);
        assert!(!old_worktree.exists());
        let old_branch = GitAdapter::worktree_branch(task.id, 1);
        let branch_check = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{old_branch}"))
            .output()
            .unwrap();
        assert!(!branch_check.status.success());
        assert_eq!(
            retried
                .recovery_history
                .last()
                .unwrap()
                .discarded_worktree
                .as_deref(),
            Some(old_worktree.to_str().unwrap())
        );
        let new_worktree = manager
            .prepare_task_worktree(project.id, task.id, &git)
            .unwrap();
        assert!(new_worktree.ends_with("attempt-2"));
        assert!(new_worktree.exists());
    }

    #[test]
    fn local_merge_requires_final_approval_and_completes_only_after_git_merge() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        init_git_fixture(&repository);
        let manager = ProjectManager::new(temp.path().join("data")).unwrap();
        let git = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Factory".to_owned(),
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
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let worktree = manager
            .prepare_task_worktree(project.id, task.id, &git)
            .unwrap();
        fs::write(worktree.join("generated.txt"), "merged output\n").unwrap();
        git_fixture(&worktree, &["add", "generated.txt"]);
        git_fixture(&worktree, &["commit", "-m", "task output"]);
        assert!(matches!(
            manager.merge_and_complete_task(project.id, task.id, &git),
            Err(ProjectError::AwaitingHumanApproval)
        ));
        complete_to_local_merge(&manager, project.id, task.id);

        // Result files under .cronos/ are locally excluded and never block the merge.
        fs::create_dir_all(worktree.join(".cronos/results")).unwrap();
        fs::write(worktree.join(".cronos/results/x.json"), "{}").unwrap();
        fs::write(repository.join("scratch.txt"), "uncommitted\n").unwrap();
        assert!(matches!(
            manager.merge_and_complete_task(project.id, task.id, &git),
            Ok(MergeOutcome::BaseDirty(_))
        ));
        let waiting = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(waiting.status, TaskStatus::InProgress);
        assert_eq!(waiting.current_step.as_deref(), Some("local-merge"));
        fs::remove_file(repository.join("scratch.txt")).unwrap();

        let Ok(MergeOutcome::Completed(completed)) =
            manager.merge_and_complete_task(project.id, task.id, &git)
        else {
            panic!("expected a completed merge");
        };
        assert_eq!(completed.status, TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(repository.join("generated.txt")).unwrap(),
            "merged output\n"
        );
    }

    struct LocalMergeFixture {
        manager: ProjectManager,
        project_id: ProjectId,
        task_id: TaskId,
        repository: std::path::PathBuf,
        remote: std::path::PathBuf,
    }

    /// A task with one committed change waiting at local merge, in a repository whose `origin`
    /// is a bare repository, with the project publishing as `publish`.
    fn task_at_local_merge(root: &Path, publish: PublishMode) -> LocalMergeFixture {
        let repository = root.join("repo");
        let remote = root.join("remote.git");
        init_git_fixture(&repository);
        git_fixture(root, &["init", "--bare", remote.to_str().unwrap()]);
        git_fixture(
            &repository,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let manager = ProjectManager::new(root.join("data")).unwrap();
        let git = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Factory".to_owned(),
                None,
                Some(&repository),
                Some(vec!["true".to_owned()]),
                None,
                &git,
            )
            .unwrap();
        manager
            .set_project_publish(project.id, publish, None)
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let worktree = manager
            .prepare_task_worktree(project.id, task.id, &git)
            .unwrap();
        fs::write(worktree.join("generated.txt"), "published output\n").unwrap();
        git_fixture(&worktree, &["add", "generated.txt"]);
        git_fixture(&worktree, &["commit", "-m", "task output"]);
        complete_to_local_merge(&manager, project.id, task.id);
        LocalMergeFixture {
            manager,
            project_id: project.id,
            task_id: task.id,
            repository,
            remote,
        }
    }

    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn local_publishing_never_pushes() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::Local);

        let Ok(MergeOutcome::Completed(task)) = fixture.manager.merge_and_complete_task(
            fixture.project_id,
            fixture.task_id,
            &GitAdapter::default(),
        ) else {
            panic!("expected a completed merge");
        };

        assert_eq!(task.published, None);
        assert_eq!(git_output(&fixture.remote, &["branch", "--list"]), "");
    }

    #[test]
    fn push_publishing_merges_then_pushes_and_a_failed_push_can_be_retried() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::Push);
        let git = GitAdapter::default();
        let base = git.current_branch(&fixture.repository).unwrap();
        // Break the remote so the first push fails after the local merge succeeded.
        git_fixture(
            &fixture.repository,
            &[
                "remote",
                "set-url",
                "origin",
                "/definitely/missing/remote.git",
            ],
        );

        let error = fixture
            .manager
            .merge_and_complete_task(fixture.project_id, fixture.task_id, &git)
            .unwrap_err();
        assert!(
            matches!(error, ProjectError::Git(GitError::PushFailed(_))),
            "{error}"
        );
        let failed = fixture
            .manager
            .get_task(fixture.project_id, fixture.task_id)
            .unwrap();
        assert_eq!(failed.status, TaskStatus::Error);
        assert_eq!(failed.current_step.as_deref(), Some("local-merge"));
        assert!(
            failed
                .agent_activity
                .last()
                .unwrap()
                .detail
                .as_deref()
                .unwrap()
                .contains("Publishing failed")
        );

        git_fixture(
            &fixture.repository,
            &[
                "remote",
                "set-url",
                "origin",
                fixture.remote.to_str().unwrap(),
            ],
        );
        fixture
            .manager
            .retry_step(fixture.project_id, fixture.task_id)
            .unwrap();
        let Ok(MergeOutcome::Completed(task)) =
            fixture
                .manager
                .merge_and_complete_task(fixture.project_id, fixture.task_id, &git)
        else {
            panic!("expected the retried publish to complete");
        };

        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(
            task.published.as_deref(),
            Some(format!("pushed {base} to origin").as_str())
        );
        assert_eq!(
            git_output(&fixture.remote, &["rev-parse", &base]),
            git_output(&fixture.repository, &["rev-parse", "HEAD"])
        );
        // The retry pushed the existing merge instead of merging a second time.
        assert_eq!(
            git_output(
                &fixture.repository,
                &["rev-list", "--merges", "--count", "HEAD"]
            ),
            "1"
        );
    }

    fn commit_in(worktree: &Path, file: &str, message: &str) {
        fs::write(worktree.join(file), format!("{message}\n")).unwrap();
        git_fixture(worktree, &["add", file]);
        git_fixture(worktree, &["commit", "-m", message]);
    }

    #[test]
    fn releases_tag_the_merge_and_write_the_changelog() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::Local);
        fixture
            .manager
            .set_project_releases(fixture.project_id, true)
            .unwrap();
        let worktree = fixture
            .manager
            .validated_task_worktree_path(fixture.project_id, fixture.task_id)
            .unwrap();
        commit_in(&worktree, "parser.txt", "feat(parser): add output");

        let Ok(MergeOutcome::Completed(task)) = fixture.manager.merge_and_complete_task(
            fixture.project_id,
            fixture.task_id,
            &GitAdapter::default(),
        ) else {
            panic!("expected a completed merge");
        };

        assert_eq!(task.release.as_deref(), Some("v0.1.0"));
        let changelog = fs::read_to_string(fixture.repository.join("CHANGELOG.md")).unwrap();
        assert!(
            changelog.starts_with("# Changelog\n\n## v0.1.0 - "),
            "{changelog}"
        );
        assert!(changelog.contains("### Features\n\n- **parser:** add output"));
        assert!(changelog.contains("- task output"));
        assert_eq!(
            git_output(&fixture.repository, &["log", "-1", "--format=%s"]),
            "chore(release): v0.1.0"
        );
        assert_eq!(
            git_output(&fixture.repository, &["tag", "--points-at", "HEAD"]),
            "v0.1.0"
        );
        assert_eq!(
            git_output(&fixture.repository, &["status", "--porcelain"]),
            ""
        );
    }

    #[test]
    fn a_retried_push_reuses_the_release_and_pushes_its_tag() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::Push);
        fixture
            .manager
            .set_project_releases(fixture.project_id, true)
            .unwrap();
        git_fixture(&fixture.repository, &["tag", "v1.2.3", "HEAD"]);
        git_fixture(
            &fixture.repository,
            &[
                "remote",
                "set-url",
                "origin",
                "/definitely/missing/remote.git",
            ],
        );
        let git = GitAdapter::default();
        assert!(
            fixture
                .manager
                .merge_and_complete_task(fixture.project_id, fixture.task_id, &git)
                .is_err()
        );

        git_fixture(
            &fixture.repository,
            &[
                "remote",
                "set-url",
                "origin",
                fixture.remote.to_str().unwrap(),
            ],
        );
        fixture
            .manager
            .retry_step(fixture.project_id, fixture.task_id)
            .unwrap();
        let Ok(MergeOutcome::Completed(task)) =
            fixture
                .manager
                .merge_and_complete_task(fixture.project_id, fixture.task_id, &git)
        else {
            panic!("expected the retry to complete");
        };

        assert_eq!(task.release.as_deref(), Some("v1.2.4"));
        assert_eq!(
            git_output(&fixture.repository, &["tag", "--list", "v*"]),
            "v1.2.3\nv1.2.4",
            "the retry adopted the release instead of making another"
        );
        assert_eq!(
            git_output(&fixture.remote, &["rev-parse", "v1.2.4^{commit}"]),
            git_output(&fixture.repository, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn projects_without_releases_write_no_changelog() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::Local);

        let Ok(MergeOutcome::Completed(task)) = fixture.manager.merge_and_complete_task(
            fixture.project_id,
            fixture.task_id,
            &GitAdapter::default(),
        ) else {
            panic!("expected a completed merge");
        };

        assert_eq!(task.release, None);
        assert!(!fixture.repository.join("CHANGELOG.md").exists());
        assert_eq!(git_output(&fixture.repository, &["tag"]), "");
    }

    #[cfg(unix)]
    #[test]
    fn pull_request_publishing_opens_a_request_instead_of_merging_locally() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let fixture = task_at_local_merge(temp.path(), PublishMode::PullRequest);
        let gh = temp.path().join("gh");
        fs::write(
            &gh,
            "#!/bin/sh\n[ \"$1 $2\" = \"pr create\" ] && printf 'https://example.invalid/pull/3\\n' || exit 1\n",
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        let git = GitAdapter::with_tools(crate::integrations::ToolCommands::default().with_gh(gh));
        let head_before = git_output(&fixture.repository, &["rev-parse", "HEAD"]);

        let Ok(MergeOutcome::Completed(task)) =
            fixture
                .manager
                .merge_and_complete_task(fixture.project_id, fixture.task_id, &git)
        else {
            panic!("expected the pull request to complete the task");
        };

        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(
            task.published.as_deref(),
            Some("https://example.invalid/pull/3")
        );
        assert_eq!(
            git_output(&fixture.repository, &["rev-parse", "HEAD"]),
            head_before
        );
        assert!(!git_output(&fixture.remote, &["branch", "--list"]).is_empty());
    }

    #[test]
    fn merge_conflict_returns_the_task_to_code_generation_without_final_approval() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        init_git_fixture(&repository);
        let manager = ProjectManager::new(temp.path().join("data")).unwrap();
        let git = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Factory".to_owned(),
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
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let worktree = manager
            .prepare_task_worktree(project.id, task.id, &git)
            .unwrap();
        fs::write(worktree.join("README.md"), "task version\n").unwrap();
        git_fixture(&worktree, &["commit", "-am", "task change"]);
        fs::write(repository.join("README.md"), "base version\n").unwrap();
        git_fixture(&repository, &["commit", "-am", "base change"]);
        complete_to_local_merge(&manager, project.id, task.id);

        let Ok(MergeOutcome::Conflict(task)) =
            manager.merge_and_complete_task(project.id, task.id, &git)
        else {
            panic!("expected a merge conflict");
        };
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.current_step.as_deref(), Some("code-generation"));
        assert_eq!(task.automatic_revisions, 1);
        assert!(!task.is_approved(ApprovalGate::FinalCode));
        assert!(task.is_approved(ApprovalGate::Specifications));
        assert!(
            task.agent_activity
                .last()
                .unwrap()
                .detail
                .as_deref()
                .unwrap()
                .contains("conflicted")
        );
        assert_eq!(
            fs::read_to_string(repository.join("README.md")).unwrap(),
            "base version\n"
        );
    }

    #[test]
    fn worktree_requires_committed_openspec_directory() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git_fixture(&repository, &["init"]);
        git_fixture(&repository, &["config", "user.name", "Cronos Tests"]);
        git_fixture(
            &repository,
            &["config", "user.email", "cronos@example.invalid"],
        );
        fs::write(repository.join("README.md"), "base\n").unwrap();
        git_fixture(&repository, &["add", "README.md"]);
        git_fixture(&repository, &["commit", "-m", "initial"]);
        // An untracked openspec/ is not enough: worktrees only contain committed files.
        fs::create_dir_all(repository.join("openspec")).unwrap();
        let manager = ProjectManager::new(temp.path().join("data")).unwrap();
        let git = GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Factory".to_owned(),
                None,
                Some(&repository),
                None,
                None,
                &git,
            )
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        assert!(matches!(
            manager.prepare_task_worktree(project.id, task.id, &git),
            Err(ProjectError::OpenSpecNotInitialized)
        ));
        let task = manager.get_task(project.id, task.id).unwrap();
        assert!(task.worktree.is_none());
        let common = repository.join(".git/info/exclude");
        assert!(
            fs::read_to_string(common)
                .unwrap()
                .lines()
                .any(|line| line == ".cronos/")
        );
    }

    #[test]
    fn retry_step_keeps_attempt_approvals_and_step_and_resume_finds_interrupted_agents() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
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
        for (agent, status) in [
            ("builder", AgentStatus::Starting),
            ("helper", AgentStatus::Starting),
            ("helper", AgentStatus::Completed),
        ] {
            manager
                .record_agent_activity(
                    project.id,
                    task.id,
                    agent.to_owned(),
                    "code-generation".to_owned(),
                    status,
                    None,
                )
                .unwrap();
        }
        assert_eq!(
            manager.interrupted_agents(project.id, task.id).unwrap(),
            ["builder"]
        );
        assert!(matches!(
            manager.retry_step(project.id, task.id),
            Err(ProjectError::TaskNotInError(TaskStatus::InProgress))
        ));
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        let resumed = manager.retry_step(project.id, task.id).unwrap();
        assert_eq!(resumed.status, TaskStatus::InProgress);
        assert_eq!(resumed.attempt, 1);
        assert_eq!(resumed.current_step.as_deref(), Some("code-generation"));
        assert!(resumed.is_approved(ApprovalGate::Specifications));
        assert_eq!(
            resumed.recovery_history.last().unwrap().action,
            RecoveryAction::RetryStep
        );
    }

    #[test]
    fn project_crud_and_archive_persist() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        assert_eq!(manager.select_project(project.id).unwrap(), project);

        let updated = manager
            .update_project(project.id, Some("Cronos".to_owned()), None)
            .unwrap();
        assert_eq!(updated.name, "Cronos");
        assert_eq!(manager.list_projects().unwrap(), vec![updated.clone()]);

        let archived = manager.archive_project(project.id).unwrap();
        assert_eq!(archived.status, ProjectStatus::Archived);
        assert!(matches!(
            manager.select_project(project.id),
            Err(ProjectError::ArchivedProject)
        ));
        assert_eq!(
            manager.list_projects().unwrap()[0].status,
            ProjectStatus::Archived
        );
    }

    #[test]
    fn task_creation_validates_dependencies_and_prioritizes_ready_work() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let first = manager
            .create_task(
                project.id,
                "First".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();
        let blocked = manager
            .create_task(
                project.id,
                "Blocked".to_owned(),
                None,
                Priority::High,
                vec![first.id],
            )
            .unwrap();
        let ready_high = manager
            .create_task(
                project.id,
                "High priority".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(first.status, TaskStatus::Pending);
        let ready = manager.ready_tasks(project.id).unwrap();
        assert_eq!(ready[0].id, ready_high.id);
        assert!(ready.iter().any(|task| task.id == first.id));
        assert!(!ready.iter().any(|task| task.id == blocked.id));

        let selected = manager.start_next_task(project.id).unwrap().unwrap();
        assert_eq!(selected.id, ready_high.id);
        assert!(manager.start_next_task(project.id).unwrap().is_none());
        complete_for_queue(&manager, project.id, ready_high.id);
        complete_for_queue(&manager, project.id, first.id);
        assert!(
            manager
                .ready_tasks(project.id)
                .unwrap()
                .iter()
                .any(|task| task.id == blocked.id)
        );

        let other_project = manager.create_project("Other".to_owned(), None).unwrap();
        assert!(matches!(
            manager.create_task(
                other_project.id,
                "Invalid dependency".to_owned(),
                None,
                Priority::Medium,
                vec![first.id]
            ),
            Err(ProjectError::UnknownDependency(_))
        ));
    }

    #[test]
    fn project_must_exist_to_be_selected() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        assert!(manager.select_project(ProjectId::new()).is_err());
    }

    #[test]
    fn agent_activity_is_persisted_only_for_running_tasks_and_bounded() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Monitor workers".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        assert!(matches!(
            manager.record_agent_activity(
                project.id,
                task.id,
                "coder".to_owned(),
                "code-generation".to_owned(),
                AgentStatus::Running,
                None,
            ),
            Err(ProjectError::TaskNotInProgress(TaskStatus::Pending))
        ));
        manager
            .set_task_status(project.id, task.id, TaskStatus::InProgress)
            .unwrap();

        for index in 0..=MAX_AGENT_EVENTS {
            manager
                .record_agent_activity(
                    project.id,
                    task.id,
                    format!("agent-{index}"),
                    "code-generation".to_owned(),
                    AgentStatus::Running,
                    Some("working".to_owned()),
                )
                .unwrap();
        }
        let stored = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(stored.agent_activity.len(), MAX_AGENT_EVENTS);
        assert_eq!(stored.agent_activity[0].agent_name, "agent-1");
        assert_eq!(
            stored.agent_activity.last().unwrap().agent_name,
            "agent-100"
        );
    }

    #[test]
    fn task_worktree_must_match_its_assigned_directory() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Worktree".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut stored = manager.get_task(project.id, task.id).unwrap();
        stored.worktree = Some(root.path().join("outside").to_string_lossy().into_owned());
        manager.store.save_task(&stored).unwrap();
        assert!(matches!(
            manager.validated_task_worktree_path(project.id, task.id),
            Err(ProjectError::InvalidTaskWorktree)
        ));

        let expected = manager
            .store
            .expected_task_worktree_path(project.id, task.id, task.attempt);
        std::fs::create_dir_all(&expected).unwrap();
        stored.worktree = Some(expected.to_string_lossy().into_owned());
        manager.store.save_task(&stored).unwrap();
        assert_eq!(
            manager
                .validated_task_worktree_path(project.id, task.id)
                .unwrap(),
            expected
        );
    }

    #[test]
    fn retry_starts_a_new_attempt_and_does_not_reuse_the_old_worktree_or_approval() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Retry me".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
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
            .record_agent_activity(
                project.id,
                task.id,
                "coder".to_owned(),
                "code-generation".to_owned(),
                AgentStatus::Running,
                None,
            )
            .unwrap();
        let old_worktree = manager
            .store
            .expected_task_worktree_path(project.id, task.id, 1);
        std::fs::create_dir_all(&old_worktree).unwrap();
        let mut stored = manager.get_task(project.id, task.id).unwrap();
        stored.worktree = Some(old_worktree.to_string_lossy().into_owned());
        manager.store.save_task(&stored).unwrap();
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        assert!(matches!(
            manager.set_task_status(project.id, task.id, TaskStatus::InProgress),
            Err(ProjectError::UseRecoveryAction)
        ));

        let resumed = manager.retry_task(project.id, task.id).unwrap();
        assert_eq!(resumed.attempt, 2);
        assert_eq!(resumed.status, TaskStatus::InProgress);
        assert_eq!(
            resumed.current_step.as_deref(),
            Some("specification-generation")
        );
        assert!(resumed.worktree.is_none());
        assert!(!resumed.is_approved(ApprovalGate::Specifications));
        assert_eq!(resumed.agent_activity[0].attempt, 1);
        assert_eq!(resumed.recovery_history[0].action, RecoveryAction::Retry);
        assert_eq!(
            resumed.recovery_history[0].discarded_worktree.as_deref(),
            Some(old_worktree.to_str().unwrap())
        );
        assert_ne!(
            manager
                .store
                .expected_task_worktree_path(project.id, task.id, resumed.attempt),
            old_worktree
        );
    }

    #[test]
    fn clarification_recovery_requires_context_and_restarts_spec_generation() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Clarify me".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        assert!(matches!(
            manager.clarify_task(project.id, task.id, "  ".to_owned()),
            Err(ProjectError::ClarificationRequired)
        ));
        let resumed = manager
            .clarify_task(project.id, task.id, "Handle offline mode".to_owned())
            .unwrap();
        assert_eq!(resumed.status, TaskStatus::InProgress);
        assert_eq!(resumed.attempt, 2);
        assert_eq!(
            resumed.current_step.as_deref(),
            Some("specification-generation")
        );
        assert_eq!(resumed.recovery_history[0].action, RecoveryAction::Clarify);
        assert_eq!(
            resumed.recovery_history[0].note.as_deref(),
            Some("Handle offline mode")
        );
    }

    #[test]
    fn task_abandonment_is_terminal_and_requires_a_reason() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Abandon me".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();
        assert!(matches!(
            manager.abandon_task(project.id, task.id, "  ".to_owned()),
            Err(ProjectError::AbandonmentNoteRequired)
        ));
        assert!(matches!(
            manager.set_task_status(project.id, task.id, TaskStatus::Rejected),
            Err(ProjectError::UseAbandonTask)
        ));
        let rejected = manager
            .abandon_task(project.id, task.id, "No longer needed".to_owned())
            .unwrap();
        assert_eq!(rejected.status, TaskStatus::Rejected);
        assert_eq!(
            rejected.abandonment_note.as_deref(),
            Some("No longer needed")
        );
    }

    #[test]
    fn task_state_commands_enforce_transitions_and_record_steps() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Factory".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Implement workflow".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();

        assert!(matches!(
            manager.set_task_step(project.id, task.id, "specs".to_owned()),
            Err(ProjectError::TaskNotInProgress(TaskStatus::Pending))
        ));
        manager
            .set_task_status(project.id, task.id, TaskStatus::InProgress)
            .unwrap();
        assert!(matches!(
            manager.complete_task(project.id, task.id),
            Err(ProjectError::CompletionNotReady)
        ));
        assert!(matches!(
            manager.set_task_status(project.id, task.id, TaskStatus::Completed),
            Err(ProjectError::UseCompleteTask)
        ));
        let in_step = manager
            .set_task_step(project.id, task.id, "specification-generation".to_owned())
            .unwrap();
        assert_eq!(
            in_step.current_step.as_deref(),
            Some("specification-generation")
        );
        assert!(
            in_step
                .step_started_at_unix_secs
                .contains_key("specification-generation")
        );
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        let resumed = manager.retry_task(project.id, task.id).unwrap();
        assert_eq!(resumed.status, TaskStatus::InProgress);
        assert_eq!(resumed.attempt, 2);
        assert_eq!(
            resumed.current_step.as_deref(),
            Some("specification-generation")
        );
        assert!(matches!(
            manager.set_task_step(project.id, task.id, "code-generation".to_owned()),
            Err(ProjectError::ApprovalRequired(ApprovalGate::Specifications))
        ));
        manager
            .set_task_step(project.id, task.id, "specification-approval".to_owned())
            .unwrap();
        assert!(matches!(
            manager.record_approval(
                project.id,
                task.id,
                ApprovalGate::Specifications,
                ApprovalDecision::Rejected,
                None,
            ),
            Err(ProjectError::ApprovalNoteRequired)
        ));
        let revised = manager
            .record_approval(
                project.id,
                task.id,
                ApprovalGate::Specifications,
                ApprovalDecision::Rejected,
                Some("Add an explicit acceptance criterion".to_owned()),
            )
            .unwrap();
        assert_eq!(revised.status, TaskStatus::InProgress);
        assert_eq!(
            revised.current_step.as_deref(),
            Some("specification-generation")
        );
        assert_eq!(
            revised.approval_history.last().unwrap().note.as_deref(),
            Some("Add an explicit acceptance criterion")
        );

        manager
            .set_task_step(project.id, task.id, "specification-approval".to_owned())
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
            manager.set_task_step(project.id, task.id, "documentation".to_owned()),
            Err(ProjectError::ApprovalRequired(ApprovalGate::FinalCode))
        ));
        for step in ["code-review", "verification", "final-approval"] {
            manager
                .set_task_step(project.id, task.id, step.to_owned())
                .unwrap();
        }
        let revised = manager
            .record_approval(
                project.id,
                task.id,
                ApprovalGate::FinalCode,
                ApprovalDecision::Rejected,
                Some("Fix the failed integration test".to_owned()),
            )
            .unwrap();
        assert_eq!(revised.status, TaskStatus::InProgress);
        assert_eq!(revised.current_step.as_deref(), Some("code-generation"));
        assert!(!revised.is_approved(ApprovalGate::FinalCode));
        manager
            .set_task_step(project.id, task.id, "code-generation".to_owned())
            .unwrap();
        for step in ["code-review", "verification", "final-approval"] {
            manager
                .set_task_step(project.id, task.id, step.to_owned())
                .unwrap();
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
        manager
            .set_task_step(project.id, task.id, "local-merge".to_owned())
            .unwrap();
        let completed = manager.complete_task(project.id, task.id).unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(matches!(
            manager.set_task_status(project.id, task.id, TaskStatus::Error),
            Err(ProjectError::InvalidStatusTransition {
                from: TaskStatus::Completed,
                to: TaskStatus::Error
            })
        ));
    }
}
