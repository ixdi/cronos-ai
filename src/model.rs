use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MAX_NAME_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 10_000;
const MAX_STEP_NAME_CHARS: usize = 100;
const MAX_AGENT_NAME_CHARS: usize = 100;
const MAX_AGENT_DETAIL_CHARS: usize = 2_000;
const MAX_APPROVAL_NOTE_CHARS: usize = 2_000;
const MAX_WORKTREE_PATH_CHARS: usize = 4_096;
pub const MAX_AGENT_EVENTS: usize = 100;
pub const MAX_APPROVAL_RECORDS: usize = 100;
pub const MAX_RECOVERY_RECORDS: usize = 50;
pub const MAX_CHECK_COMMAND_ARGS: usize = 64;
const MAX_CHECK_COMMAND_ARG_CHARS: usize = 4_096;
/// Check-command time limit used when a project does not set its own.
pub const DEFAULT_CHECK_TIMEOUT_MINUTES: u32 = 60;
/// Longest check-command time limit a project can set, matching the agent profile limit.
pub const MAX_CHECK_TIMEOUT_MINUTES: u32 = 480;
const DEFAULT_REMOTE: &str = "origin";
const MAX_REMOTE_CHARS: usize = 100;
const MAX_PUBLICATION_CHARS: usize = 2_000;

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

/// What happens after a task's final code is approved and documented.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishMode {
    /// Merge into the base branch locally; nothing leaves the machine.
    #[default]
    Local,
    /// Merge locally, then push the base branch to the project's remote.
    Push,
    /// Push the task branch and open a pull request instead of merging locally.
    PullRequest,
}

impl PublishMode {
    pub const ALL: [Self; 3] = [Self::Local, Self::Push, Self::PullRequest];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Push => "push",
            Self::PullRequest => "pull-request",
        }
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
    #[serde(default)]
    pub repository: Option<String>,
    /// Program and arguments run without a shell in the task worktree during verification.
    #[serde(default)]
    pub check_command: Option<Vec<String>>,
    /// Minutes the check command may run before it is stopped; `None` uses the default.
    #[serde(default)]
    pub check_timeout_minutes: Option<u32>,
    #[serde(default)]
    pub publish: PublishMode,
    /// Git remote used for publishing; `None` means `origin`.
    #[serde(default)]
    pub remote: Option<String>,
    /// Whether each locally merged task gets a release tag and a changelog entry.
    #[serde(default)]
    pub releases: bool,
    pub created_at_unix_secs: u64,
    pub updated_at_unix_secs: u64,
}

impl Project {
    pub fn check_timeout(&self) -> std::time::Duration {
        let minutes = self
            .check_timeout_minutes
            .unwrap_or(DEFAULT_CHECK_TIMEOUT_MINUTES);
        std::time::Duration::from_secs(u64::from(minutes) * 60)
    }

    pub fn remote_name(&self) -> &str {
        self.remote.as_deref().unwrap_or(DEFAULT_REMOTE)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self
            .remote
            .as_deref()
            .is_some_and(|remote| !valid_remote_name(remote))
        {
            return Err(ModelError::InvalidRemote);
        }
        if self
            .check_timeout_minutes
            .is_some_and(|minutes| minutes == 0 || minutes > MAX_CHECK_TIMEOUT_MINUTES)
        {
            return Err(ModelError::InvalidCheckTimeout);
        }
        validate_required_text("project name", &self.name, MAX_NAME_CHARS)?;
        validate_optional_text(
            "project description",
            self.description.as_deref(),
            MAX_DESCRIPTION_CHARS,
        )?;
        validate_optional_text(
            "project repository",
            self.repository.as_deref(),
            MAX_WORKTREE_PATH_CHARS,
        )?;
        if let Some(command) = &self.check_command {
            if command.is_empty() || command.len() > MAX_CHECK_COMMAND_ARGS {
                return Err(ModelError::InvalidCheckCommand);
            }
            for argument in command {
                validate_optional_text(
                    "check command argument",
                    Some(argument),
                    MAX_CHECK_COMMAND_ARG_CHARS,
                )?;
                if argument.contains('\0') {
                    return Err(ModelError::InvalidCheckCommand);
                }
            }
            validate_required_text(
                "check command program",
                &command[0],
                MAX_CHECK_COMMAND_ARG_CHARS,
            )?;
        }

        let mut task_ids = HashSet::with_capacity(self.task_ids.len());
        if self.task_ids.iter().any(|id| !task_ids.insert(id)) {
            return Err(ModelError::DuplicateTaskId);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Running,
    Waiting,
    Completed,
    ChangesRequested,
    Failed,
    Cancelled,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentActivity {
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    pub agent_name: String,
    pub step: String,
    pub status: AgentStatus,
    pub detail: Option<String>,
    pub updated_at_unix_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStep {
    SpecificationGeneration,
    SpecificationApproval,
    CodeGeneration,
    CodeReview,
    Verification,
    FinalApproval,
    Documentation,
    LocalMerge,
}

impl WorkflowStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpecificationGeneration => "specification-generation",
            Self::SpecificationApproval => "specification-approval",
            Self::CodeGeneration => "code-generation",
            Self::CodeReview => "code-review",
            Self::Verification => "verification",
            Self::FinalApproval => "final-approval",
            Self::Documentation => "documentation",
            Self::LocalMerge => "local-merge",
        }
    }

    pub fn parse(step: &str) -> Option<Self> {
        Some(match step {
            "specification-generation" => Self::SpecificationGeneration,
            "specification-approval" => Self::SpecificationApproval,
            "code-generation" => Self::CodeGeneration,
            "code-review" => Self::CodeReview,
            "verification" => Self::Verification,
            "final-approval" => Self::FinalApproval,
            "documentation" => Self::Documentation,
            "local-merge" => Self::LocalMerge,
            _ => return None,
        })
    }

    pub fn next(self) -> Option<Self> {
        Some(match self {
            Self::SpecificationGeneration => Self::SpecificationApproval,
            Self::SpecificationApproval => Self::CodeGeneration,
            Self::CodeGeneration => Self::CodeReview,
            Self::CodeReview => Self::Verification,
            Self::Verification => Self::FinalApproval,
            Self::FinalApproval => Self::Documentation,
            Self::Documentation => Self::LocalMerge,
            Self::LocalMerge => return None,
        })
    }

    pub fn requires_human_approval(self) -> bool {
        matches!(self, Self::SpecificationApproval | Self::FinalApproval)
    }

    pub fn runs_agents(self) -> bool {
        matches!(
            self,
            Self::SpecificationGeneration
                | Self::CodeGeneration
                | Self::CodeReview
                | Self::Verification
                | Self::Documentation
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalGate {
    Specifications,
    FinalCode,
}

impl ApprovalGate {
    pub fn approval_step(self) -> &'static str {
        match self {
            Self::Specifications => "specification-approval",
            Self::FinalCode => "final-approval",
        }
    }

    pub fn revision_step(self) -> &'static str {
        match self {
            Self::Specifications => "specification-generation",
            Self::FinalCode => "code-generation",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalRecord {
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    pub gate: ApprovalGate,
    pub decision: ApprovalDecision,
    pub note: Option<String>,
    pub decided_at_unix_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Retry,
    Clarify,
    RetryStep,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryRecord {
    pub attempt: u32,
    pub action: RecoveryAction,
    pub note: Option<String>,
    pub discarded_worktree: Option<String>,
    pub resumed_at_unix_secs: u64,
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
    #[serde(default)]
    pub agent_activity: Vec<AgentActivity>,
    #[serde(default)]
    pub approval_history: Vec<ApprovalRecord>,
    #[serde(default)]
    pub abandonment_note: Option<String>,
    pub worktree: Option<String>,
    pub created_at_unix_secs: u64,
    pub updated_at_unix_secs: u64,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    #[serde(default)]
    pub recovery_history: Vec<RecoveryRecord>,
    /// Automatic returns to code generation in the current attempt.
    #[serde(default)]
    pub automatic_revisions: u32,
    /// Where the completed work was published, such as a pull request URL.
    #[serde(default)]
    pub published: Option<String>,
    /// The release this task's merge was tagged as, such as `v1.4.0`.
    #[serde(default)]
    pub release: Option<String>,
}

impl Task {
    /// Name of the single OpenSpec change that holds this task's specifications.
    pub fn openspec_change(&self) -> String {
        format!("cronos-{}", &self.id.0.simple().to_string()[..8])
    }

    pub fn is_approved(&self, gate: ApprovalGate) -> bool {
        self.approval_history
            .iter()
            .rev()
            .find(|record| record.attempt == self.attempt && record.gate == gate)
            .is_some_and(|record| record.decision == ApprovalDecision::Approved)
    }

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
        validate_optional_text(
            "task publication",
            self.published.as_deref(),
            MAX_PUBLICATION_CHARS,
        )?;
        if self
            .release
            .as_deref()
            .is_some_and(|release| crate::release::Version::parse(release).is_none())
        {
            return Err(ModelError::InvalidRelease);
        }

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
        if self.agent_activity.len() > MAX_AGENT_EVENTS {
            return Err(ModelError::TooManyAgentEvents);
        }
        if self.approval_history.len() > MAX_APPROVAL_RECORDS {
            return Err(ModelError::TooManyApprovalRecords);
        }
        if self.recovery_history.len() > MAX_RECOVERY_RECORDS {
            return Err(ModelError::TooManyRecoveryRecords);
        }
        validate_optional_text(
            "task abandonment note",
            self.abandonment_note.as_deref(),
            MAX_APPROVAL_NOTE_CHARS,
        )?;
        validate_optional_text(
            "task worktree path",
            self.worktree.as_deref(),
            MAX_WORKTREE_PATH_CHARS,
        )?;
        for activity in &self.agent_activity {
            validate_required_text("agent name", &activity.agent_name, MAX_AGENT_NAME_CHARS)?;
            validate_required_text("agent step", &activity.step, MAX_STEP_NAME_CHARS)?;
            validate_optional_text(
                "agent activity detail",
                activity.detail.as_deref(),
                MAX_AGENT_DETAIL_CHARS,
            )?;
        }
        for recovery in &self.recovery_history {
            validate_optional_text(
                "recovery note",
                recovery.note.as_deref(),
                MAX_APPROVAL_NOTE_CHARS,
            )?;
            validate_optional_text(
                "discarded worktree path",
                recovery.discarded_worktree.as_deref(),
                MAX_WORKTREE_PATH_CHARS,
            )?;
        }
        for approval in &self.approval_history {
            validate_optional_text(
                "approval note",
                approval.note.as_deref(),
                MAX_APPROVAL_NOTE_CHARS,
            )?;
            if approval.decision == ApprovalDecision::Rejected
                && approval
                    .note
                    .as_deref()
                    .is_none_or(|note| note.trim().is_empty())
            {
                return Err(ModelError::MissingApprovalNote);
            }
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
    #[error("task contains more than {MAX_AGENT_EVENTS} agent activity events")]
    TooManyAgentEvents,
    #[error("task contains more than {MAX_APPROVAL_RECORDS} approval records")]
    TooManyApprovalRecords,
    #[error("rejected approvals must include a revision note")]
    MissingApprovalNote,
    #[error("task contains more than {MAX_RECOVERY_RECORDS} recovery records")]
    TooManyRecoveryRecords,
    #[error("check command must have a program and at most {MAX_CHECK_COMMAND_ARGS} arguments")]
    InvalidCheckCommand,
    #[error("check time limit must be between 1 and {MAX_CHECK_TIMEOUT_MINUTES} minutes")]
    InvalidCheckTimeout,
    #[error(
        "remote name must be 1-{MAX_REMOTE_CHARS} letters, digits, '.', '_', or '-', and must not start with '-' or '.'"
    )]
    InvalidRemote,
    #[error("a task release must be a version tag such as v1.2.3")]
    InvalidRelease,
}

fn default_attempt() -> u32 {
    1
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

/// A Git remote name that can never be read as an option, path, or shell syntax.
fn valid_remote_name(remote: &str) -> bool {
    !remote.is_empty()
        && remote.len() <= MAX_REMOTE_CHARS
        && !remote.starts_with(['-', '.'])
        && remote
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
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
        DEFAULT_CHECK_TIMEOUT_MINUTES, MAX_CHECK_TIMEOUT_MINUTES, ModelError, Priority, Project,
        ProjectId, ProjectStatus, PublishMode, Task, TaskId, TaskStatus,
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
            agent_activity: Vec::new(),
            approval_history: Vec::new(),
            abandonment_note: None,
            worktree: None,
            attempt: 1,
            recovery_history: Vec::new(),
            automatic_revisions: 0,
            published: None,
            release: None,
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
            repository: None,
            check_command: None,
            check_timeout_minutes: None,
            publish: Default::default(),
            remote: None,
            releases: false,
            created_at_unix_secs: 0,
            updated_at_unix_secs: 0,
        };

        assert_eq!(project.validate(), Err(ModelError::DuplicateTaskId));
        project.task_ids.pop();
        project.check_command = Some(Vec::new());
        assert_eq!(project.validate(), Err(ModelError::InvalidCheckCommand));
        project.check_command = Some(vec![" ".to_owned()]);
        assert_eq!(
            project.validate(),
            Err(ModelError::BlankText("check command program"))
        );
        project.check_command = Some(vec!["./scripts/check.sh".to_owned()]);
        assert_eq!(project.validate(), Ok(()));
        project.name = "  ".to_owned();
        assert_eq!(
            project.validate(),
            Err(ModelError::BlankText("project name"))
        );
    }

    #[test]
    fn check_timeout_defaults_to_an_hour_and_must_stay_in_range() {
        let mut project = Project {
            id: ProjectId::new(),
            name: "Cronos project".to_owned(),
            description: None,
            status: ProjectStatus::Active,
            task_ids: Vec::new(),
            repository: None,
            check_command: None,
            check_timeout_minutes: None,
            publish: Default::default(),
            remote: None,
            releases: false,
            created_at_unix_secs: 0,
            updated_at_unix_secs: 0,
        };
        assert_eq!(
            project.check_timeout(),
            std::time::Duration::from_secs(DEFAULT_CHECK_TIMEOUT_MINUTES as u64 * 60)
        );
        assert_eq!(DEFAULT_CHECK_TIMEOUT_MINUTES, 60);

        for minutes in [1, MAX_CHECK_TIMEOUT_MINUTES] {
            project.check_timeout_minutes = Some(minutes);
            assert_eq!(project.validate(), Ok(()));
            assert_eq!(
                project.check_timeout(),
                std::time::Duration::from_secs(u64::from(minutes) * 60)
            );
        }
        for minutes in [0, MAX_CHECK_TIMEOUT_MINUTES + 1] {
            project.check_timeout_minutes = Some(minutes);
            assert_eq!(project.validate(), Err(ModelError::InvalidCheckTimeout));
        }
    }

    #[test]
    fn publishing_defaults_to_local_on_origin_and_rejects_unsafe_remote_names() {
        let mut project: Project = serde_json::from_value(serde_json::json!({
            "id": ProjectId::new(),
            "name": "Published",
            "description": null,
            "status": "active",
            "task_ids": [],
            "created_at_unix_secs": 1,
            "updated_at_unix_secs": 1
        }))
        .unwrap();
        assert_eq!(project.publish, PublishMode::Local);
        assert_eq!(project.remote_name(), "origin");
        assert!(!project.releases, "releases are opt-in");

        project.publish = PublishMode::PullRequest;
        project.remote = Some("upstream".to_owned());
        assert_eq!(project.validate(), Ok(()));
        assert_eq!(project.remote_name(), "upstream");
        assert_eq!(
            serde_json::to_value(PublishMode::PullRequest).unwrap(),
            "pull_request"
        );

        for remote in ["", " ", "-c", "my remote", "a;b", "../x"] {
            project.remote = Some(remote.to_owned());
            assert_eq!(
                project.validate(),
                Err(ModelError::InvalidRemote),
                "{remote:?}"
            );
        }
    }

    #[test]
    fn task_publication_is_bounded() {
        let mut task = task(TaskId::new(), Vec::new());
        assert_eq!(task.published, None);
        assert_eq!(task.release, None);
        task.release = Some("v0.1.0".to_owned());
        assert_eq!(task.validate(), Ok(()));
        task.release = Some("latest".to_owned());
        assert_eq!(task.validate(), Err(ModelError::InvalidRelease));
        task.release = None;
        task.published = Some("https://example.invalid/pr/1".to_owned());
        assert_eq!(task.validate(), Ok(()));
        task.published = Some("x".repeat(2_001));
        assert!(matches!(
            task.validate(),
            Err(ModelError::TextTooLong {
                field: "task publication",
                ..
            })
        ));
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
    fn legacy_project_without_repository_deserializes() {
        let project: Project = serde_json::from_value(serde_json::json!({
            "id": ProjectId::new(),
            "name": "Legacy",
            "description": null,
            "status": "active",
            "task_ids": [],
            "created_at_unix_secs": 1,
            "updated_at_unix_secs": 1
        }))
        .unwrap();
        assert_eq!(project.repository, None);
        assert_eq!(project.check_command, None);
        assert_eq!(project.check_timeout_minutes, None);
    }

    #[test]
    fn legacy_task_without_agent_activity_deserializes_with_empty_activity() {
        let mut value = serde_json::to_value(task(TaskId::new(), Vec::new())).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("agent_activity");
        object.remove("approval_history");
        object.remove("abandonment_note");
        object.remove("attempt");
        object.remove("recovery_history");
        object.remove("automatic_revisions");
        let task: Task = serde_json::from_value(value).unwrap();
        assert!(task.agent_activity.is_empty());
        assert_eq!(task.automatic_revisions, 0);
        assert!(task.openspec_change().starts_with("cronos-"));
        assert_eq!(task.openspec_change().len(), "cronos-".len() + 8);
    }

    #[test]
    fn task_validation_rejects_blank_step_names() {
        let mut task = task(TaskId::new(), Vec::new());
        task.step_started_at_unix_secs.insert(" ".to_owned(), 1);
        assert_eq!(task.validate(), Err(ModelError::InvalidStepName));
    }
}
