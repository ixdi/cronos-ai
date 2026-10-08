use std::{
    collections::HashSet,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    model::{ModelError, Priority, Project, ProjectId, ProjectStatus, Task, TaskId, TaskStatus},
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
        let now = now_unix_secs()?;
        task.current_step = Some(step.clone());
        task.step_started_at_unix_secs.insert(step, now);
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
    use crate::model::{Priority, ProjectId, ProjectStatus, TaskStatus};

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

        manager
            .set_task_status(project.id, first.id, TaskStatus::InProgress)
            .unwrap();
        manager
            .set_task_status(project.id, first.id, TaskStatus::Completed)
            .unwrap();
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
        manager
            .set_task_status(project.id, task.id, TaskStatus::Completed)
            .unwrap();
        assert!(matches!(
            manager.set_task_status(project.id, task.id, TaskStatus::Error),
            Err(ProjectError::InvalidStatusTransition {
                from: TaskStatus::Completed,
                to: TaskStatus::Error
            })
        ));
    }
}
