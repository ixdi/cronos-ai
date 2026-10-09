use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    integrations::{GitAdapter, GitError},
    model::{
        AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, ApprovalRecord,
        MAX_AGENT_EVENTS, MAX_APPROVAL_RECORDS, MAX_RECOVERY_RECORDS, ModelError, Priority,
        Project, ProjectId, ProjectStatus, RecoveryAction, RecoveryRecord, Task, TaskId,
        TaskStatus, WorkflowStep,
    },
    storage::{ProjectStore, StoreError},
};

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

    pub fn create_project(
        &self,
        name: String,
        description: Option<String>,
    ) -> Result<Project, ProjectError> {
        self.create_project_with_repository(name, description, None, &GitAdapter::default())
    }

    pub fn create_project_with_repository(
        &self,
        name: String,
        description: Option<String>,
        repository: Option<&Path>,
        git: &GitAdapter,
    ) -> Result<Project, ProjectError> {
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
        let mut project = self.store.load_project(id)?;
        project.status = ProjectStatus::Archived;
        project.updated_at_unix_secs = now_unix_secs()?;
        self.store.save_project(&project)?;
        Ok(project)
    }

    pub fn create_task(
        &self,
        project_id: ProjectId,
        title: String,
        description: Option<String>,
        priority: Priority,
        dependencies: Vec<TaskId>,
    ) -> Result<Task, ProjectError> {
        let mut project = self.store.load_project(project_id)?;
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
        git.add_worktree(&repository, &worktree, &branch)?;
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

    pub fn merge_and_complete_task(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        git: &GitAdapter,
    ) -> Result<Task, ProjectError> {
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
        git.merge_worktree(
            &repository,
            &worktree,
            &GitAdapter::worktree_branch(task.id, task.attempt),
        )?;
        self.complete_workflow_step(project_id, task_id, WorkflowStep::LocalMerge)
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
    use super::{ProjectError, ProjectManager};
    use crate::integrations::GitAdapter;
    use crate::model::{
        AgentStatus, ApprovalDecision, ApprovalGate, MAX_AGENT_EVENTS, Priority, ProjectId,
        ProjectStatus, RecoveryAction, TaskStatus, WorkflowStep,
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
        git_fixture(root, &["add", "README.md"]);
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
            .create_project_with_repository("Factory".to_owned(), None, Some(&repository), &git)
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
            .create_project_with_repository("Factory".to_owned(), None, Some(&repository), &git)
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

        let completed = manager
            .merge_and_complete_task(project.id, task.id, &git)
            .unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(repository.join("generated.txt")).unwrap(),
            "merged output\n"
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
