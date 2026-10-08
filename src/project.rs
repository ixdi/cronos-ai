use std::{
    collections::HashSet,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    model::{
        AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, ApprovalRecord,
        MAX_AGENT_EVENTS, MAX_APPROVAL_RECORDS, ModelError, Priority, Project, ProjectId,
        ProjectStatus, Task, TaskId, TaskStatus,
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

    pub fn create_project(
        &self,
        name: String,
        description: Option<String>,
    ) -> Result<Project, ProjectError> {
        let now = now_unix_secs()?;
        let project = Project {
            id: ProjectId::new(),
            name,
            description,
            status: ProjectStatus::Active,
            task_ids: Vec::new(),
            created_at_unix_secs: now,
            updated_at_unix_secs: now,
        };
        project.validate()?;
        self.store.save_project(&project)?;
        Ok(project)
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

    pub fn set_task_step(
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
    use crate::model::{
        AgentStatus, ApprovalDecision, ApprovalGate, MAX_AGENT_EVENTS, Priority, ProjectId,
        ProjectStatus, TaskStatus,
    };

    fn complete_for_queue(
        manager: &ProjectManager,
        project_id: ProjectId,
        task_id: crate::model::TaskId,
    ) {
        manager
            .set_task_status(project_id, task_id, TaskStatus::InProgress)
            .unwrap();
        manager
            .set_task_step(project_id, task_id, "specification-approval".to_owned())
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
            "code-generation",
            "code-review",
            "verification",
            "final-approval",
        ] {
            manager
                .set_task_step(project_id, task_id, step.to_owned())
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
            .set_task_step(project_id, task_id, "local-merge".to_owned())
            .unwrap();
        manager.complete_task(project_id, task_id).unwrap();
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
            .set_task_step(project.id, task.id, "spec-generation".to_owned())
            .unwrap();
        assert_eq!(in_step.current_step.as_deref(), Some("spec-generation"));
        assert!(
            in_step
                .step_started_at_unix_secs
                .contains_key("spec-generation")
        );
        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        manager
            .set_task_status(project.id, task.id, TaskStatus::InProgress)
            .unwrap();
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
