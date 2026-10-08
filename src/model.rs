use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MAX_NAME_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 10_000;
const MAX_STEP_NAME_CHARS: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProjectId(pub Uuid);

// Keep ID creation explicit; a random `Default` would hide identity creation.
#[allow(clippy::new_without_default)]
impl ProjectId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TaskId(pub Uuid);

// Keep ID creation explicit; a random `Default` would hide identity creation.
#[allow(clippy::new_without_default)]
impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Archived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Rejected,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub task_ids: Vec<TaskId>,
    pub created_at_unix_secs: u64,
    pub updated_at_unix_secs: u64,
}

impl Project {
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_required_text("project name", &self.name, MAX_NAME_CHARS)?;
        validate_optional_text(
            "project description",
            self.description.as_deref(),
            MAX_DESCRIPTION_CHARS,
        )?;

        let mut task_ids = HashSet::with_capacity(self.task_ids.len());
        if self.task_ids.iter().any(|id| !task_ids.insert(id)) {
            return Err(ModelError::DuplicateTaskId);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Task {
    pub id: TaskId,
    pub project_id: ProjectId,
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    pub dependencies: Vec<TaskId>,
    pub status: TaskStatus,
    pub current_step: Option<String>,
    pub step_started_at_unix_secs: BTreeMap<String, u64>,
    pub worktree: Option<String>,
    pub created_at_unix_secs: u64,
    pub updated_at_unix_secs: u64,
}

impl Task {
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_required_text("task title", &self.title, MAX_NAME_CHARS)?;
        validate_optional_text(
            "task description",
            self.description.as_deref(),
            MAX_DESCRIPTION_CHARS,
        )?;
        validate_optional_text(
            "current workflow step",
            self.current_step.as_deref(),
            MAX_STEP_NAME_CHARS,
        )?;

        let mut dependencies = HashSet::with_capacity(self.dependencies.len());
        for dependency in &self.dependencies {
            if *dependency == self.id {
                return Err(ModelError::SelfDependency);
            }
            if !dependencies.insert(dependency) {
                return Err(ModelError::DuplicateDependency);
            }
        }

        if self
            .step_started_at_unix_secs
            .keys()
            .any(|step| step.trim().is_empty() || step.chars().count() > MAX_STEP_NAME_CHARS)
        {
            return Err(ModelError::InvalidStepName);
        }

        Ok(())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModelError {
    #[error("{0} must not be blank")]
    BlankText(&'static str),
    #[error("{field} exceeds the {max}-character limit")]
    TextTooLong { field: &'static str, max: usize },
    #[error("project contains duplicate task IDs")]
    DuplicateTaskId,
    #[error("task cannot depend on itself")]
    SelfDependency,
    #[error("task contains duplicate dependencies")]
    DuplicateDependency,
    #[error("workflow step names must be nonblank and at most {MAX_STEP_NAME_CHARS} characters")]
    InvalidStepName,
}

fn validate_required_text(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::BlankText(field));
    }
    if value.chars().count() > max_chars {
        return Err(ModelError::TextTooLong {
            field,
            max: max_chars,
        });
    }
    Ok(())
}

fn validate_optional_text(
    field: &'static str,
    value: Option<&str>,
    max_chars: usize,
) -> Result<(), ModelError> {
    if value.is_some_and(|value| value.chars().count() > max_chars) {
        return Err(ModelError::TextTooLong {
            field,
            max: max_chars,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ModelError, Priority, Project, ProjectId, ProjectStatus, Task, TaskId, TaskStatus,
    };
    use std::collections::BTreeMap;

    fn task(id: TaskId, dependencies: Vec<TaskId>) -> Task {
        Task {
            id,
            project_id: ProjectId::new(),
            title: "Build feature".to_owned(),
            description: None,
            priority: Priority::Medium,
            dependencies,
            status: TaskStatus::Pending,
            current_step: None,
            step_started_at_unix_secs: BTreeMap::new(),
            worktree: None,
            created_at_unix_secs: 0,
            updated_at_unix_secs: 0,
        }
    }

    #[test]
    fn project_validation_rejects_blank_and_duplicate_values() {
        let duplicate_id = TaskId::new();
        let mut project = Project {
            id: ProjectId::new(),
            name: "Cronos project".to_owned(),
            description: None,
            status: ProjectStatus::Active,
            task_ids: vec![duplicate_id, duplicate_id],
            created_at_unix_secs: 0,
            updated_at_unix_secs: 0,
        };

        assert_eq!(project.validate(), Err(ModelError::DuplicateTaskId));
        project.name = "  ".to_owned();
        assert_eq!(
            project.validate(),
            Err(ModelError::BlankText("project name"))
        );
    }

    #[test]
    fn task_validation_rejects_self_and_duplicate_dependencies() {
        let id = TaskId::new();
        assert_eq!(
            task(id, vec![id]).validate(),
            Err(ModelError::SelfDependency)
        );

        let dependency = TaskId::new();
        assert_eq!(
            task(TaskId::new(), vec![dependency, dependency]).validate(),
            Err(ModelError::DuplicateDependency)
        );
    }

    #[test]
    fn task_validation_rejects_blank_step_names() {
        let mut task = task(TaskId::new(), Vec::new());
        task.step_started_at_unix_secs.insert(" ".to_owned(), 1);
        assert_eq!(task.validate(), Err(ModelError::InvalidStepName));
    }
}
