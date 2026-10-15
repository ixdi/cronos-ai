use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};

mod profiles;

use profiles::{EditorAction, ProfileEditor, ProfileList};

use crate::{
    agent_profiles::{load_profiles_for_editing, save_edited_profiles},
    integrations::{GitAdapter, PiRpcHandle, ToolCommands, herdr_session_available},
    model::{
        AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, Priority, Project,
        ProjectStatus, Task, TaskId, TaskStatus, WorkflowStep,
    },
    project::{MAX_AUTOMATIC_REVISIONS, MergeOutcome, ProjectError, ProjectManager},
    workflow::{cancel_interrupted_agents, check_program_changed},
};

const PRIMARY: Color = Color::Rgb(188, 147, 248);
const WARNING: Color = Color::Rgb(255, 184, 108);
const ERROR: Color = Color::Rgb(254, 86, 84);
const SHORTCUT: Color = Color::Rgb(222, 110, 177);
const SUCCESS: Color = Color::Rgb(0, 106, 99);
// Neutral tones for labels, borders, and the selected row; readable on light and dark terminals.
const MUTED: Color = Color::Rgb(138, 138, 160);
const SELECTION: Color = Color::Rgb(62, 52, 92);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Projects,
    Tasks,
    TaskDetail,
    Review,
    Profiles,
    ProfileEditor,
}

enum Dialog {
    None,
    DeleteProfile { id: String },
    Revision { gate: ApprovalGate, note: String },
    Clarify { note: String },
    Abandon { note: String },
    CreateProject(ProjectForm),
    CreateTask(TaskForm),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProjectField {
    Name,
    Description,
    Repository,
    CheckCommand,
    CheckTimeout,
}

impl ProjectField {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Description,
            Self::Description => Self::Repository,
            Self::Repository => Self::CheckCommand,
            Self::CheckCommand => Self::CheckTimeout,
            Self::CheckTimeout => Self::Name,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Name => Self::CheckTimeout,
            Self::Description => Self::Name,
            Self::Repository => Self::Description,
            Self::CheckCommand => Self::Repository,
            Self::CheckTimeout => Self::CheckCommand,
        }
    }
}

struct ProjectForm {
    name: String,
    description: String,
    repository: String,
    check_command: String,
    check_timeout: String,
    focus: ProjectField,
}

impl Default for ProjectForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            repository: String::new(),
            check_command: String::new(),
            check_timeout: String::new(),
            focus: ProjectField::Name,
        }
    }
}

impl ProjectForm {
    fn focused_value_mut(&mut self) -> &mut String {
        match self.focus {
            ProjectField::Name => &mut self.name,
            ProjectField::Description => &mut self.description,
            ProjectField::Repository => &mut self.repository,
            ProjectField::CheckCommand => &mut self.check_command,
            ProjectField::CheckTimeout => &mut self.check_timeout,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TaskField {
    Title,
    Description,
    Priority,
    Dependencies,
}

impl TaskField {
    fn next(self) -> Self {
        match self {
            Self::Title => Self::Description,
            Self::Description => Self::Priority,
            Self::Priority => Self::Dependencies,
            Self::Dependencies => Self::Title,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Title => Self::Dependencies,
            Self::Description => Self::Title,
            Self::Priority => Self::Description,
            Self::Dependencies => Self::Priority,
        }
    }
}

struct TaskForm {
    title: String,
    description: String,
    priority: Priority,
    dependencies: Vec<TaskId>,
    focus: TaskField,
    dependency_cursor: usize,
}

impl Default for TaskForm {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            priority: Priority::Medium,
            dependencies: Vec::new(),
            focus: TaskField::Title,
            dependency_cursor: 0,
        }
    }
}

impl TaskForm {
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            TaskField::Title => Some(&mut self.title),
            TaskField::Description => Some(&mut self.description),
            TaskField::Priority | TaskField::Dependencies => None,
        }
    }

    fn toggle_dependency(&mut self, id: TaskId) {
        if let Some(index) = self
            .dependencies
            .iter()
            .position(|selected| *selected == id)
        {
            self.dependencies.remove(index);
        } else {
            self.dependencies.push(id);
        }
    }
}

struct Dashboard {
    manager: ProjectManager,
    projects: Vec<Project>,
    tasks: Vec<Task>,
    screen: Screen,
    selected_project: usize,
    selected_task: usize,
    selected_activity: usize,
    should_quit: bool,
    error: Option<String>,
    dialog: Dialog,
    workflow: Option<PiRpcHandle>,
    workflow_task: Option<(crate::model::ProjectId, crate::model::TaskId)>,
    workflow_status: Option<String>,
    review_text: Vec<String>,
    review_scroll: u16,
    profile_list: ProfileList,
    profile_editor: Option<ProfileEditor>,
}

impl Dashboard {
    fn new(manager: ProjectManager) -> Result<Self, ProjectError> {
        Ok(Self {
            projects: manager.list_projects()?,
            manager,
            tasks: Vec::new(),
            screen: Screen::Projects,
            selected_project: 0,
            selected_task: 0,
            selected_activity: 0,
            should_quit: false,
            error: None,
            dialog: Dialog::None,
            workflow: None,
            workflow_task: None,
            workflow_status: None,
            review_text: Vec::new(),
            review_scroll: 0,
            profile_list: ProfileList::default(),
            profile_editor: None,
        })
    }

    fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.selected_project)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected_task)
    }

    fn refresh(&mut self) -> Result<(), ProjectError> {
        match self.screen {
            Screen::Profiles => {
                self.profile_list.reload(self.manager.data_dir());
                return Ok(());
            }
            // Never reload under an open editor; it holds unsaved changes.
            Screen::ProfileEditor => return Ok(()),
            _ => {}
        }
        let project_id = self.selected_project().map(|project| project.id);
        let task_id = self.selected_task().map(|task| task.id);
        let follow_latest_activity = self.selected_task().is_none_or(|task| {
            self.selected_activity >= task.agent_activity.len().saturating_sub(1)
        });
        self.projects = self.manager.list_projects()?;
        self.selected_project = project_id
            .and_then(|id| self.projects.iter().position(|project| project.id == id))
            .unwrap_or_else(|| {
                self.selected_project
                    .min(self.projects.len().saturating_sub(1))
            });

        if self.screen == Screen::Projects {
            self.tasks.clear();
        } else if let Some(project) = self.selected_project() {
            self.tasks = self.manager.list_tasks(project.id)?;
            self.selected_task = task_id
                .and_then(|id| self.tasks.iter().position(|task| task.id == id))
                .unwrap_or_else(|| self.selected_task.min(self.tasks.len().saturating_sub(1)));
            if matches!(self.screen, Screen::TaskDetail | Screen::Review) && self.tasks.is_empty() {
                self.screen = Screen::Tasks;
            }
        } else {
            self.tasks.clear();
            self.screen = Screen::Projects;
        }
        self.selected_activity = self.selected_task().map_or(0, |task| {
            if follow_latest_activity {
                task.agent_activity.len().saturating_sub(1)
            } else {
                self.selected_activity
                    .min(task.agent_activity.len().saturating_sub(1))
            }
        });
        Ok(())
    }

    fn refresh_for_ui(&mut self) {
        if let Err(error) = self.refresh() {
            self.error = Some(error.to_string());
        }
    }

    fn handle_key(&mut self, key: KeyCode) {
        if !matches!(&self.dialog, Dialog::None) {
            self.handle_dialog_key(key);
            return;
        }
        // Errors stay visible across the periodic refresh until the user acts again.
        self.error = None;
        if self.screen == Screen::ProfileEditor {
            self.handle_profile_editor_key(key);
            return;
        }
        if key == KeyCode::Char('q') {
            self.should_quit = true;
            return;
        }
        if key == KeyCode::Char('r')
            && self.screen == Screen::TaskDetail
            && self
                .selected_task()
                .is_some_and(|task| task.status == TaskStatus::Error)
        {
            self.retry_error_task();
            return;
        }
        if key == KeyCode::Char('r') {
            self.refresh_for_ui();
            return;
        }
        if matches!(key, KeyCode::Esc | KeyCode::Backspace) {
            match self.screen {
                Screen::Projects => self.should_quit = true,
                Screen::Tasks => {
                    self.screen = Screen::Projects;
                    self.refresh_for_ui();
                }
                Screen::TaskDetail => {
                    self.screen = Screen::Tasks;
                    self.refresh_for_ui();
                }
                Screen::Review => self.screen = Screen::TaskDetail,
                Screen::Profiles => {
                    self.screen = Screen::Projects;
                    self.refresh_for_ui();
                }
                Screen::ProfileEditor => {}
            }
            return;
        }

        if self.screen == Screen::Review {
            match key {
                KeyCode::Up => self.review_scroll = self.review_scroll.saturating_sub(1),
                KeyCode::Down => {
                    let max = u16::try_from(self.review_text.len()).unwrap_or(u16::MAX);
                    self.review_scroll = self.review_scroll.saturating_add(1).min(max);
                }
                KeyCode::PageUp => self.review_scroll = self.review_scroll.saturating_sub(10),
                KeyCode::PageDown => {
                    let max = u16::try_from(self.review_text.len()).unwrap_or(u16::MAX);
                    self.review_scroll = self.review_scroll.saturating_add(10).min(max);
                }
                _ => {}
            }
            return;
        }

        if self.screen == Screen::TaskDetail {
            match key {
                KeyCode::Char('y') => self.approve_active_gate(),
                KeyCode::Char('n') => self.request_revision(),
                KeyCode::Char('c') => self.open_clarify_dialog(),
                KeyCode::Char('a') => self.open_abandon_dialog(),
                KeyCode::Char('t') => self.retry_failed_step(),
                KeyCode::Char('v') => self.open_review(),
                _ => self.handle_navigation_key(key),
            }
            return;
        }
        self.handle_navigation_key(key);
    }

    fn handle_navigation_key(&mut self, key: KeyCode) {
        match (self.screen, key) {
            (Screen::Projects, KeyCode::Char('n')) => {
                self.error = None;
                self.dialog = Dialog::CreateProject(ProjectForm::default());
            }
            (Screen::Projects, KeyCode::Char('p')) => {
                self.screen = Screen::Profiles;
                self.profile_list.selected = 0;
                self.refresh_for_ui();
            }
            (Screen::Profiles, KeyCode::Up | KeyCode::Down) => {
                self.profile_list.move_selection(key == KeyCode::Down);
            }
            (Screen::Profiles, KeyCode::Char('n')) if self.profile_list.load_error().is_none() => {
                self.profile_editor = Some(ProfileEditor::new());
                self.screen = Screen::ProfileEditor;
            }
            (Screen::Profiles, KeyCode::Enter | KeyCode::Char('e')) => {
                if let Some(profile) = self.profile_list.selected() {
                    self.profile_editor = Some(ProfileEditor::edit(profile));
                    self.screen = Screen::ProfileEditor;
                }
            }
            (Screen::Profiles, KeyCode::Char('d')) => {
                if let Some(profile) = self.profile_list.selected() {
                    self.dialog = Dialog::DeleteProfile {
                        id: profile.id.clone(),
                    };
                }
            }
            (Screen::Projects, KeyCode::Up) => {
                self.selected_project = self.selected_project.saturating_sub(1);
            }
            (Screen::Projects, KeyCode::Down) => {
                self.selected_project = self
                    .selected_project
                    .saturating_add(1)
                    .min(self.projects.len().saturating_sub(1));
            }
            (Screen::Projects, KeyCode::Enter) if self.selected_project().is_some() => {
                self.screen = Screen::Tasks;
                self.selected_task = 0;
                self.refresh_for_ui();
            }
            (Screen::Tasks, KeyCode::Char('n')) => {
                if self
                    .selected_project()
                    .is_some_and(|project| project.status == ProjectStatus::Active)
                {
                    self.error = None;
                    self.dialog = Dialog::CreateTask(TaskForm::default());
                } else {
                    self.error = Some("Archived projects cannot receive tasks.".to_owned());
                }
            }
            (Screen::Tasks, KeyCode::Up) => {
                self.selected_task = self.selected_task.saturating_sub(1);
            }
            (Screen::Tasks, KeyCode::Down) => {
                self.selected_task = self
                    .selected_task
                    .saturating_add(1)
                    .min(self.tasks.len().saturating_sub(1));
            }
            (Screen::Tasks, KeyCode::Enter) if self.selected_task().is_some() => {
                self.screen = Screen::TaskDetail;
                self.selected_activity = self
                    .selected_task()
                    .map(|task| task.agent_activity.len().saturating_sub(1))
                    .unwrap_or(0);
            }
            (Screen::Tasks, KeyCode::Char('s')) => self.start_next_workflow(),
            (Screen::TaskDetail, KeyCode::Up) => {
                self.selected_activity = self.selected_activity.saturating_sub(1);
            }
            (Screen::TaskDetail, KeyCode::Down) => {
                self.selected_activity = self
                    .selected_task()
                    .map(|task| {
                        self.selected_activity
                            .saturating_add(1)
                            .min(task.agent_activity.len().saturating_sub(1))
                    })
                    .unwrap_or(0);
            }
            _ => {}
        }
    }

    fn active_approval_gate(&self) -> Option<ApprovalGate> {
        match self.selected_task()?.current_step.as_deref()? {
            "specification-approval" => Some(ApprovalGate::Specifications),
            "final-approval" => Some(ApprovalGate::FinalCode),
            _ => None,
        }
    }

    fn approve_active_gate(&mut self) {
        if self.workflow.is_some() {
            self.error = Some("Wait for the Pi workflow to settle before approving.".to_owned());
            return;
        }
        let Some(gate) = self.active_approval_gate() else {
            self.error = Some("No approval gate is active for this task.".to_owned());
            return;
        };
        self.save_approval(gate, ApprovalDecision::Approved, None);
    }

    fn request_revision(&mut self) {
        if self.workflow.is_some() {
            self.error =
                Some("Wait for the Pi workflow to settle before requesting revisions.".to_owned());
            return;
        }
        let Some(gate) = self.active_approval_gate() else {
            self.error = Some("No approval gate is active for this task.".to_owned());
            return;
        };
        self.error = None;
        self.dialog = Dialog::Revision {
            gate,
            note: String::new(),
        };
    }

    fn retry_error_task(&mut self) {
        if self.workflow.is_some() {
            self.error = Some("Wait for the Pi workflow to settle before retrying.".to_owned());
            return;
        }
        let Some((project_id, task_id)) = self.selected_ids() else {
            return;
        };
        if !self.stop_stale_agents(project_id, task_id) {
            return;
        }
        match self
            .manager
            .retry_task_with_git(project_id, task_id, &GitAdapter::default())
        {
            Ok(task) => {
                self.workflow_status = Some(format!(
                    "Retry scheduled for attempt {} at specification generation",
                    task.attempt
                ));
                self.resume_recovered_task(project_id, task_id);
                self.refresh_for_ui();
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn retry_failed_step(&mut self) {
        if self.workflow.is_some() {
            self.error = Some("Wait for the Pi workflow to settle before retrying.".to_owned());
            return;
        }
        let Some((project_id, task_id)) = self.selected_ids() else {
            return;
        };
        if !self.stop_stale_agents(project_id, task_id) {
            return;
        }
        match self.manager.retry_step(project_id, task_id) {
            Ok(task) => {
                self.workflow_status = Some(format!(
                    "Retrying {} in attempt {}",
                    task.current_step.as_deref().unwrap_or("the failed step"),
                    task.attempt
                ));
                self.continue_task(project_id, task_id);
                self.refresh_for_ui();
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn handle_profile_editor_key(&mut self, key: KeyCode) {
        let Some(editor) = self.profile_editor.as_mut() else {
            self.screen = Screen::Profiles;
            return;
        };
        match editor.handle_key(key) {
            EditorAction::None => {}
            EditorAction::Cancel => self.close_profile_editor(),
            EditorAction::Save => self.save_profile_editor(),
        }
    }

    fn close_profile_editor(&mut self) {
        self.profile_editor = None;
        self.screen = Screen::Profiles;
        self.refresh_for_ui();
    }

    /// Saves the edited profile into the catalog as it is on disk now, so edits made elsewhere
    /// since the editor opened are kept.
    fn save_profile_editor(&mut self) {
        let Some(editor) = self.profile_editor.as_ref() else {
            return;
        };
        let profile = match editor.to_profile() {
            Ok(profile) => profile,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        let data_dir = self.manager.data_dir().to_path_buf();
        let result = load_profiles_for_editing(&data_dir).and_then(|mut profiles| {
            match editor
                .original_id
                .as_deref()
                .and_then(|id| profiles.iter().position(|existing| existing.id == id))
            {
                Some(index) => profiles[index] = profile.clone(),
                None => profiles.push(profile.clone()),
            }
            save_edited_profiles(&data_dir, profiles, Some(&profile.id))
        });
        match result {
            Ok(()) => {
                self.workflow_status = Some(format!("Saved agent profile {}", profile.id));
                self.close_profile_editor();
                self.profile_list.select_id(&profile.id);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn delete_profile(&mut self, id: &str) {
        let data_dir = self.manager.data_dir().to_path_buf();
        let result = load_profiles_for_editing(&data_dir).and_then(|mut profiles| {
            profiles.retain(|profile| profile.id != id);
            save_edited_profiles(&data_dir, profiles, None)
        });
        match result {
            Ok(()) => self.workflow_status = Some(format!("Deleted agent profile {id}")),
            Err(error) => self.error = Some(error.to_string()),
        }
        self.refresh_for_ui();
    }

    /// Stops agents a crashed orchestrator left running before a recovery reuses or discards the
    /// worktree. Returns false, with the reason shown, when a step process still owns the task.
    fn stop_stale_agents(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) -> bool {
        match cancel_interrupted_agents(
            &self.manager,
            &ToolCommands::default(),
            project_id,
            task_id,
        ) {
            Ok(_) => true,
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }

    /// Builds the review text: OpenSpec change, task-branch commits and diff, latest check output.
    fn open_review(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        let Some(repository) = self
            .selected_project()
            .and_then(|project| project.repository.clone())
        else {
            self.error = Some("This project has no Git repository to review.".to_owned());
            return;
        };
        let Some(worktree) = task.worktree.clone() else {
            self.error = Some("This task has no worktree to review yet.".to_owned());
            return;
        };
        let worktree = PathBuf::from(worktree);
        let change = task.openspec_change();
        let tools = ToolCommands::default();
        let spec = tools
            .run_openspec_in(&worktree, &["show", &change, "--no-interactive"])
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .or_else(|_| {
                std::fs::read_to_string(
                    worktree
                        .join("openspec/changes")
                        .join(&change)
                        .join("proposal.md"),
                )
            })
            .unwrap_or_else(|_| {
                format!("OpenSpec change {change} is not available (it may be archived).")
            });
        let git_adapter = GitAdapter::default();
        let mut git = git_adapter
            .review_summary(Path::new(&repository), &worktree)
            .unwrap_or_else(|error| format!("Could not summarize the task branch: {error}"));
        if let Some(program) = self
            .selected_project()
            .and_then(|project| project.check_command.clone())
            .and_then(|command| {
                check_program_changed(&git_adapter, Path::new(&repository), &worktree, &command)
            })
        {
            git = format!(
                "WARNING: this branch changes the check program {program}. The check ran the agent's version; review it before approving.\n\n{git}"
            );
        }
        let check = std::fs::read_to_string(worktree.join(".cronos/check-output.log"))
            .map(|output| {
                let lines = output.lines().collect::<Vec<_>>();
                lines[lines.len().saturating_sub(40)..].join("\n")
            })
            .unwrap_or_else(|_| "The check command has not run for this worktree yet.".to_owned());
        self.review_text = format!(
            "Worktree: {}\nOpenSpec change: {change}\n\n== Specification ==\n{spec}\n\n== Task branch ==\n{git}\n\n== Latest check output ==\n{check}",
            worktree.display()
        )
        .lines()
        .map(terminal_safe)
        .collect();
        self.review_scroll = 0;
        self.error = None;
        self.screen = Screen::Review;
    }

    /// Continues an In Progress task: waits at approval gates, merges at local merge,
    /// otherwise relaunches the orchestrator at the current step.
    fn continue_task(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) {
        let Ok(task) = self.manager.get_task(project_id, task_id) else {
            return;
        };
        let step = task.current_step.as_deref().and_then(WorkflowStep::parse);
        if task.status != TaskStatus::InProgress
            || step.is_some_and(WorkflowStep::requires_human_approval)
        {
            return;
        }
        if step == Some(WorkflowStep::LocalMerge) {
            self.finish_local_merge(project_id, task_id);
            return;
        }
        self.resume_recovered_task(project_id, task_id);
    }

    fn finish_local_merge(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) {
        match self
            .manager
            .merge_and_complete_task(project_id, task_id, &GitAdapter::default())
        {
            Ok(MergeOutcome::Completed(_)) => {
                self.workflow_status =
                    Some("Task merged locally and completed; nothing was pushed.".to_owned());
            }
            Ok(MergeOutcome::BaseDirty(path)) => {
                self.error = Some(format!(
                    "The base repository {path} has uncommitted changes. Commit or stash them, then press s on the task list to retry the merge."
                ));
            }
            Ok(MergeOutcome::Conflict(_)) => {
                self.workflow_status = Some(
                    "Merge conflicted and was aborted; the task returned to code generation."
                        .to_owned(),
                );
                self.resume_recovered_task(project_id, task_id);
            }
            Err(error) => {
                let _ = self
                    .manager
                    .set_task_status(project_id, task_id, TaskStatus::Error);
                self.error = Some(error.to_string());
            }
        }
        self.refresh_for_ui();
    }

    fn resume_recovered_task(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) {
        if self
            .selected_project()
            .is_none_or(|project| project.repository.is_none())
        {
            return;
        }
        let result = self
            .manager
            .prepare_task_worktree(project_id, task_id, &GitAdapter::default())
            .map_err(|error| error.to_string())
            .and_then(|_| self.launch_pi_for_task(project_id, task_id));
        if let Err(error) = result {
            let _ = self
                .manager
                .set_task_status(project_id, task_id, TaskStatus::Error);
            self.error = Some(error);
        }
    }

    fn open_clarify_dialog(&mut self) {
        if self.workflow.is_some() {
            self.error = Some("Wait for the Pi workflow to settle before clarifying.".to_owned());
            return;
        }
        if self
            .selected_task()
            .is_none_or(|task| task.status != TaskStatus::Error)
        {
            self.error = Some("Only Error tasks can be clarified.".to_owned());
            return;
        }
        self.error = None;
        self.dialog = Dialog::Clarify {
            note: String::new(),
        };
    }

    fn open_abandon_dialog(&mut self) {
        if self.workflow.is_some() {
            self.error =
                Some("Wait for the Pi workflow to settle before abandoning the task.".to_owned());
            return;
        }
        if self
            .selected_task()
            .is_some_and(|task| matches!(task.status, TaskStatus::Completed | TaskStatus::Rejected))
        {
            self.error = Some("This task is already terminal.".to_owned());
            return;
        }
        self.error = None;
        self.dialog = Dialog::Abandon {
            note: String::new(),
        };
    }

    fn handle_project_dialog_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.dialog = Dialog::None;
                self.error = None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Dialog::CreateProject(form) = &mut self.dialog {
                    form.focus = if key == KeyCode::Tab {
                        form.focus.next()
                    } else {
                        form.focus.previous()
                    };
                }
                self.error = None;
            }
            KeyCode::Backspace => {
                if let Dialog::CreateProject(form) = &mut self.dialog {
                    form.focused_value_mut().pop();
                }
                self.error = None;
            }
            KeyCode::Char(character) if !character.is_control() => {
                if let Dialog::CreateProject(form) = &mut self.dialog {
                    let value = form.focused_value_mut();
                    if value.chars().count() < 10_000 {
                        value.push(character);
                    }
                }
                self.error = None;
            }
            KeyCode::Enter => self.submit_project_creation(),
            _ => {}
        }
    }

    fn submit_project_creation(&mut self) {
        let dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        let Dialog::CreateProject(form) = dialog else {
            self.dialog = dialog;
            return;
        };
        let repository =
            (!form.repository.trim().is_empty()).then(|| PathBuf::from(form.repository.trim()));
        // shortcut: whitespace-split check command (no quoting), point it at a script for complex commands.
        let check_command = form
            .check_command
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let check_timeout = match form.check_timeout.trim() {
            "" => None,
            minutes => match minutes.parse::<u32>() {
                Ok(minutes) => Some(minutes),
                Err(_) => {
                    self.error = Some("Check limit must be a whole number of minutes.".to_owned());
                    self.dialog = Dialog::CreateProject(form);
                    return;
                }
            },
        };
        let result = self.manager.create_project_with_repository(
            form.name.clone(),
            (!form.description.trim().is_empty()).then(|| form.description.clone()),
            repository.as_deref(),
            (!check_command.is_empty()).then_some(check_command),
            check_timeout,
            &GitAdapter::default(),
        );
        match result {
            Ok(project) => {
                self.error = None;
                self.refresh_for_ui();
                self.selected_project = self
                    .projects
                    .iter()
                    .position(|item| item.id == project.id)
                    .unwrap_or(self.selected_project);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.dialog = Dialog::CreateProject(form);
            }
        }
    }

    fn handle_task_dialog_key(&mut self, key: KeyCode) {
        let task_count = self.tasks.len();
        match key {
            KeyCode::Esc => {
                self.dialog = Dialog::None;
                self.error = None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    form.focus = if key == KeyCode::Tab {
                        form.focus.next()
                    } else {
                        form.focus.previous()
                    };
                }
                self.error = None;
            }
            KeyCode::Backspace => {
                if let Dialog::CreateTask(form) = &mut self.dialog
                    && let Some(value) = form.focused_text_mut()
                {
                    value.pop();
                }
                self.error = None;
            }
            KeyCode::Char(character) if !character.is_control() => {
                let dependency = if character == ' ' {
                    match &self.dialog {
                        Dialog::CreateTask(form) if form.focus == TaskField::Dependencies => {
                            self.tasks.get(form.dependency_cursor).map(|task| task.id)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    if let Some(value) = form.focused_text_mut() {
                        if value.chars().count() < 10_000 {
                            value.push(character);
                        }
                        self.error = None;
                    } else if let Some(id) = dependency {
                        form.toggle_dependency(id);
                        self.error = None;
                    }
                }
            }
            KeyCode::Up
                if matches!(
                    &self.dialog,
                    Dialog::CreateTask(TaskForm {
                        focus: TaskField::Dependencies,
                        ..
                    })
                ) =>
            {
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    form.dependency_cursor = form.dependency_cursor.saturating_sub(1);
                }
            }
            KeyCode::Down
                if matches!(
                    &self.dialog,
                    Dialog::CreateTask(TaskForm {
                        focus: TaskField::Dependencies,
                        ..
                    })
                ) =>
            {
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    form.dependency_cursor = form
                        .dependency_cursor
                        .saturating_add(1)
                        .min(task_count.saturating_sub(1));
                }
            }
            KeyCode::Left
                if matches!(
                    &self.dialog,
                    Dialog::CreateTask(TaskForm {
                        focus: TaskField::Priority,
                        ..
                    })
                ) =>
            {
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    form.priority = match form.priority {
                        Priority::High => Priority::Medium,
                        Priority::Medium | Priority::Low => Priority::Low,
                    };
                    self.error = None;
                }
            }
            KeyCode::Right
                if matches!(
                    &self.dialog,
                    Dialog::CreateTask(TaskForm {
                        focus: TaskField::Priority,
                        ..
                    })
                ) =>
            {
                if let Dialog::CreateTask(form) = &mut self.dialog {
                    form.priority = match form.priority {
                        Priority::Low => Priority::Medium,
                        Priority::Medium | Priority::High => Priority::High,
                    };
                    self.error = None;
                }
            }
            KeyCode::Enter => self.submit_task_creation(),
            _ => {}
        }
    }

    fn submit_task_creation(&mut self) {
        let dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        let Dialog::CreateTask(form) = dialog else {
            self.dialog = dialog;
            return;
        };
        let Some(project_id) = self.selected_project().map(|project| project.id) else {
            self.error = Some("Select a project before creating a task.".to_owned());
            self.dialog = Dialog::CreateTask(form);
            return;
        };
        let result = self.manager.create_task(
            project_id,
            form.title.clone(),
            (!form.description.trim().is_empty()).then(|| form.description.clone()),
            form.priority.clone(),
            form.dependencies.clone(),
        );
        match result {
            Ok(task) => {
                self.error = None;
                self.refresh_for_ui();
                self.selected_task = self
                    .tasks
                    .iter()
                    .position(|item| item.id == task.id)
                    .unwrap_or(self.selected_task);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.dialog = Dialog::CreateTask(form);
            }
        }
    }

    fn handle_dialog_key(&mut self, key: KeyCode) {
        if let Dialog::DeleteProfile { id } = &self.dialog {
            match key {
                KeyCode::Char('y') => {
                    let id = id.clone();
                    self.dialog = Dialog::None;
                    self.delete_profile(&id);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.dialog = Dialog::None,
                _ => {}
            }
            return;
        }
        match &self.dialog {
            Dialog::CreateProject(_) => {
                self.handle_project_dialog_key(key);
                return;
            }
            Dialog::CreateTask(_) => {
                self.handle_task_dialog_key(key);
                return;
            }
            _ => {}
        }
        if key == KeyCode::Esc {
            self.dialog = Dialog::None;
            return;
        }
        if key == KeyCode::Backspace {
            match &mut self.dialog {
                Dialog::Revision { note, .. }
                | Dialog::Clarify { note }
                | Dialog::Abandon { note } => {
                    note.pop();
                }
                Dialog::None
                | Dialog::CreateProject(_)
                | Dialog::CreateTask(_)
                | Dialog::DeleteProfile { .. } => {}
            }
            return;
        }
        if let KeyCode::Char(character) = key {
            match &mut self.dialog {
                Dialog::Revision { note, .. }
                | Dialog::Clarify { note }
                | Dialog::Abandon { note }
                    if !character.is_control() && note.chars().count() < 2_000 =>
                {
                    note.push(character)
                }
                _ => {}
            }
            return;
        }
        if key != KeyCode::Enter {
            return;
        }
        if matches!(
            &self.dialog,
            Dialog::Revision { note, .. }
            | Dialog::Clarify { note }
            | Dialog::Abandon { note } if note.trim().is_empty()
        ) {
            self.error = Some("A nonblank note is required.".to_owned());
            return;
        }
        let dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        match dialog {
            Dialog::Revision { gate, note } if !note.trim().is_empty() => {
                self.save_approval(gate, ApprovalDecision::Rejected, Some(note));
            }
            Dialog::Clarify { note } if !note.trim().is_empty() => {
                if let Some((project_id, task_id)) = self.selected_ids()
                    && self.stop_stale_agents(project_id, task_id)
                {
                    match self.manager.clarify_task_with_git(
                        project_id,
                        task_id,
                        note,
                        &GitAdapter::default(),
                    ) {
                        Ok(task) => {
                            self.workflow_status = Some(format!(
                                "Clarification saved for attempt {} at specification generation",
                                task.attempt
                            ));
                            self.resume_recovered_task(project_id, task_id);
                            self.refresh_for_ui();
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            Dialog::Abandon { note } if !note.trim().is_empty() => {
                if let Some((project_id, task_id)) = self.selected_ids() {
                    match self.manager.abandon_task(project_id, task_id, note) {
                        Ok(_) => self.refresh_for_ui(),
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            Dialog::Revision { .. } => {
                self.error = Some("A revision note is required.".to_owned());
            }
            Dialog::Clarify { .. } => {
                self.error = Some("Clarification context is required.".to_owned());
            }
            Dialog::Abandon { .. } => {
                self.error = Some("An abandonment reason is required.".to_owned());
            }
            Dialog::None => {}
            Dialog::CreateProject(_) | Dialog::CreateTask(_) | Dialog::DeleteProfile { .. } => {
                unreachable!()
            }
        }
    }

    fn save_approval(
        &mut self,
        gate: ApprovalGate,
        decision: ApprovalDecision,
        note: Option<String>,
    ) {
        let Some((project_id, task_id)) = self.selected_ids() else {
            return;
        };
        match self
            .manager
            .record_approval(project_id, task_id, gate, decision, note)
        {
            Ok(_) => {
                self.refresh_for_ui();
                if let Some((project_id, task_id)) = self.selected_ids()
                    && self
                        .manager
                        .get_task(project_id, task_id)
                        .is_ok_and(|task| task.worktree.is_some())
                    && let Err(error) = self.launch_pi_for_task(project_id, task_id)
                {
                    let _ = self
                        .manager
                        .set_task_status(project_id, task_id, TaskStatus::Error);
                    self.error = Some(error);
                    self.refresh_for_ui();
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn selected_ids(&self) -> Option<(crate::model::ProjectId, crate::model::TaskId)> {
        Some((self.selected_project()?.id, self.selected_task()?.id))
    }

    fn start_next_workflow(&mut self) {
        if self.workflow.is_some() {
            self.error = Some("A Pi workflow is already running.".to_owned());
            return;
        }
        let Some(project_id) = self.selected_project().map(|project| project.id) else {
            return;
        };
        if self
            .selected_project()
            .is_none_or(|project| project.repository.is_none())
        {
            self.error = Some("Configure a Git repository before starting tasks.".to_owned());
            return;
        }
        if self
            .selected_project()
            .is_none_or(|project| project.check_command.is_none())
        {
            self.error = Some(
                "Configure a check command (cronos-ai project set-check-command) before starting tasks."
                    .to_owned(),
            );
            return;
        }
        if !herdr_session_available() {
            self.error =
                Some("Start Cronos-AI inside a Herdr-managed pane to run tasks.".to_owned());
            return;
        }
        if let Ok(tasks) = self.manager.list_tasks(project_id)
            && let Some(task) = tasks
                .iter()
                .find(|task| task.status == TaskStatus::InProgress)
        {
            self.resume_interrupted_task(project_id, task.id);
            return;
        }
        match self.manager.start_next_task(project_id) {
            Ok(Some(task)) => {
                let result = self
                    .manager
                    .prepare_task_worktree(project_id, task.id, &GitAdapter::default())
                    .map_err(|error| error.to_string())
                    .and_then(|_| self.launch_pi_for_task(project_id, task.id));
                if let Err(error) = result {
                    let _ = self
                        .manager
                        .set_task_status(project_id, task.id, TaskStatus::Error);
                    self.error = Some(error);
                    self.refresh_for_ui();
                }
            }
            Ok(None) => {
                self.error = Some("The project queue is already active or blocked.".to_owned())
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Resumes an In Progress task whose orchestrator is no longer running.
    fn resume_interrupted_task(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) {
        let Ok(task) = self.manager.get_task(project_id, task_id) else {
            return;
        };
        if task
            .current_step
            .as_deref()
            .and_then(WorkflowStep::parse)
            .is_some_and(WorkflowStep::requires_human_approval)
        {
            self.error =
                Some("The active task is waiting for your approval in its details.".to_owned());
            return;
        }
        match cancel_interrupted_agents(
            &self.manager,
            &ToolCommands::default(),
            project_id,
            task_id,
        ) {
            Ok(cancelled) => {
                self.workflow_status = Some(format!(
                    "Resuming {} ({cancelled} interrupted agents cancelled)",
                    terminal_safe(&task.title)
                ));
                self.continue_task(project_id, task_id);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.refresh_for_ui();
    }

    fn launch_pi_for_task(
        &mut self,
        project_id: crate::model::ProjectId,
        task_id: crate::model::TaskId,
    ) -> Result<(), String> {
        if !herdr_session_available() {
            return Err("Start Cronos-AI inside a Herdr-managed pane to run tasks.".to_owned());
        }
        let worktree = self
            .manager
            .validated_task_worktree_path(project_id, task_id)
            .map_err(|error| error.to_string())?;
        let extension = crate::integrations::install_workflow_extension(self.manager.data_dir())
            .map_err(|error| format!("could not install the workflow extension: {error}"))?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let commands = ToolCommands::default();
        let command = commands
            .pi_rpc_command(
                &extension,
                &worktree,
                self.manager.data_dir(),
                &executable,
                project_id,
                task_id,
            )
            .map_err(|error| error.to_string())?;
        let handle = commands
            .spawn_pi_rpc(command)
            .map_err(|error| error.to_string())?;
        self.workflow = Some(handle);
        self.workflow_task = Some((project_id, task_id));
        self.workflow_status = Some("Pi orchestrator started".to_owned());
        self.error = None;
        Ok(())
    }

    fn poll_workflow(&mut self) {
        let Some(handle) = self.workflow.as_ref() else {
            return;
        };
        while let Ok(event) = handle.events.try_recv() {
            self.workflow_status = Some(terminal_safe(&event));
        }
        let finished = handle.finished.try_recv();
        if matches!(finished, Err(std::sync::mpsc::TryRecvError::Empty)) {
            return;
        }
        self.workflow = None;
        let task_ids = self.workflow_task.take();
        match finished {
            Ok(Ok(())) => {
                if let Some((project_id, task_id)) = task_ids {
                    self.refresh_for_ui();
                    if let Ok(task) = self.manager.get_task(project_id, task_id)
                        && task.status == TaskStatus::InProgress
                    {
                        let current_step =
                            task.current_step.as_deref().and_then(WorkflowStep::parse);
                        if current_step == Some(WorkflowStep::LocalMerge) {
                            self.finish_local_merge(project_id, task_id);
                        } else if !current_step.is_some_and(WorkflowStep::requires_human_approval) {
                            let _ = self.manager.set_task_status(
                                project_id,
                                task_id,
                                TaskStatus::Error,
                            );
                            self.error = Some(
                                "Pi settled before completing the current workflow step."
                                    .to_owned(),
                            );
                            self.refresh_for_ui();
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                self.mark_workflow_error(task_ids);
                self.error = Some(error);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.mark_workflow_error(task_ids);
                self.error = Some("Pi workflow monitor disconnected unexpectedly.".to_owned());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    fn mark_workflow_error(
        &mut self,
        task_ids: Option<(crate::model::ProjectId, crate::model::TaskId)>,
    ) {
        if let Some((project_id, task_id)) = task_ids {
            if matches!(
                self.manager.get_task(project_id, task_id),
                Ok(Task {
                    status: TaskStatus::InProgress,
                    ..
                })
            ) {
                let _ = self
                    .manager
                    .set_task_status(project_id, task_id, TaskStatus::Error);
            }
            self.refresh_for_ui();
        }
    }
}

pub fn run(data_dir: PathBuf) -> Result<(), Box<dyn Error>> {
    let manager = ProjectManager::new(data_dir)?;
    let mut app = Dashboard::new(manager)?;
    let _terminal_guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut last_refresh = Instant::now();

    while !app.should_quit {
        app.poll_workflow();
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key.code),
                _ => {}
            }
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            app.refresh_for_ui();
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn draw(frame: &mut Frame, app: &Dashboard) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(frame.area());
    draw_header(frame, app, areas[0]);
    match app.screen {
        Screen::Projects => draw_projects(frame, app, areas[1]),
        Screen::Tasks => draw_tasks(frame, app, areas[1]),
        Screen::TaskDetail => draw_task_detail(frame, app, areas[1]),
        Screen::Review => draw_review(frame, app, areas[1]),
        Screen::Profiles => profiles::draw_profiles(frame, &app.profile_list, areas[1]),
        Screen::ProfileEditor => match &app.profile_editor {
            Some(editor) => profiles::draw_editor(frame, editor, areas[1]),
            None => profiles::draw_profiles(frame, &app.profile_list, areas[1]),
        },
    }
    draw_footer(frame, app, areas[2]);
    draw_dialog(frame, app);
}

/// Brand, breadcrumb of where the user is, and whether an orchestrator is running.
fn draw_header(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let mut crumbs = vec![Span::styled(
        "Cronos-AI",
        Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
    )];
    if matches!(app.screen, Screen::Profiles | Screen::ProfileEditor) {
        crumbs.push(Span::styled("  Agent profiles", Style::default().fg(MUTED)));
        if let Some(editor) = app
            .profile_editor
            .as_ref()
            .filter(|_| app.screen == Screen::ProfileEditor)
        {
            crumbs.push(Span::styled(" › ", Style::default().fg(MUTED)));
            crumbs.push(Span::styled(
                editor.title(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
    } else {
        crumbs.push(Span::styled("  Projects", Style::default().fg(MUTED)));
    }
    if matches!(
        app.screen,
        Screen::Tasks | Screen::TaskDetail | Screen::Review
    ) && let Some(project) = app.selected_project()
    {
        crumbs.push(Span::styled(" › ", Style::default().fg(MUTED)));
        crumbs.push(Span::raw(terminal_safe(&project.name)));
    }
    if matches!(app.screen, Screen::TaskDetail | Screen::Review)
        && let Some(task) = app.selected_task()
    {
        crumbs.push(Span::styled(" › ", Style::default().fg(MUTED)));
        crumbs.push(Span::styled(
            terminal_safe(&task.title),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    if app.screen == Screen::Review {
        crumbs.push(Span::styled(" › review", Style::default().fg(MUTED)));
    }
    let orchestrator = if app.workflow.is_some() {
        Span::styled("● orchestrator running ", Style::default().fg(PRIMARY))
    } else {
        Span::styled("○ orchestrator idle ", Style::default().fg(MUTED))
    };
    let width = orchestrator.width() as u16;
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(width)])
        .split(area);
    frame.render_widget(Paragraph::new(Line::from(crumbs)), columns[0]);
    frame.render_widget(Paragraph::new(Line::from(orchestrator)), columns[1]);
}

/// Message line (error or orchestrator status) above the key hints for the current screen.
fn draw_footer(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let help = match app.screen {
        Screen::Projects => {
            "↑/↓ select  Enter open  n new project  p agent profiles  r refresh  q quit"
        }
        Screen::Profiles => {
            "↑/↓ select  n new  Enter edit  d delete  Backspace projects  r refresh  q quit"
        }
        Screen::ProfileEditor => app
            .profile_editor
            .as_ref()
            .map_or("Esc back", ProfileEditor::hints),
        Screen::Tasks => {
            "↑/↓ select  Enter details  n new task  s start/resume  Backspace projects  r refresh  q quit"
        }
        Screen::TaskDetail
            if app
                .selected_task()
                .is_some_and(|task| task.status == TaskStatus::Error) =>
        {
            "t retry step  r retry + discard  c clarify + discard  v review  a abandon  Backspace tasks  q quit"
        }
        Screen::TaskDetail if app.active_approval_gate().is_some() => {
            "v review  y approve  n request revision  a abandon  ↑/↓ activity  Backspace tasks  q quit"
        }
        Screen::TaskDetail => {
            "v review  a abandon  ↑/↓ activity  Backspace tasks  r refresh  q quit"
        }
        Screen::Review => "↑/↓ scroll  PgUp/PgDn page  Backspace details  q quit",
    };
    let message = if let Some(error) = &app.error {
        Line::from(Span::styled(
            format!("✕ Error: {}", terminal_safe(error)),
            Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
        ))
    } else if let Some(status) = &app.workflow_status {
        Line::from(vec![
            Span::styled("◆ ", Style::default().fg(PRIMARY)),
            Span::styled(terminal_safe(status), Style::default().fg(MUTED)),
        ])
    } else {
        Line::default()
    };
    frame.render_widget(
        Paragraph::new(vec![message, shortcut_help_line(help)]),
        area,
    );
}

/// Rounded panel with a padded, highlighted title.
fn panel(title: impl Into<String>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTED))
        .title(Span::styled(
            format!(" {} ", title.into()),
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
}

fn table_header(labels: &[&'static str]) -> Row<'static> {
    Row::new(labels.iter().map(|label| Cell::from(*label)))
        .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD))
}

/// Background and weight only, so status colors stay visible on the selected row.
fn selected_row_style() -> Style {
    Style::default().bg(SELECTION).add_modifier(Modifier::BOLD)
}

/// Centered hint for an empty list, with the key that fixes it highlighted.
fn draw_empty_state(frame: &mut Frame, block: Block<'static>, area: Rect, message: Line<'static>) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let row = Rect {
        y: inner.y + inner.height / 2,
        height: 1.min(inner.height),
        ..inner
    };
    frame.render_widget(Paragraph::new(message).alignment(Alignment::Center), row);
}

fn key_hint(before: &str, key: &str, after: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(before.to_owned(), Style::default().fg(MUTED)),
        Span::styled(
            key.to_owned(),
            Style::default().fg(SHORTCUT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(after.to_owned(), Style::default().fg(MUTED)),
    ])
}

fn draw_projects(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let block = panel("Projects");
    if app.projects.is_empty() {
        draw_empty_state(
            frame,
            block,
            area,
            key_hint("No projects yet. Press ", "n", " to create one."),
        );
        return;
    }
    let rows = app.projects.iter().map(|project| {
        let status = match project.status {
            ProjectStatus::Active => "● active",
            ProjectStatus::Archived => "◌ archived",
        };
        let repository = project.repository.as_deref().map_or_else(
            || Span::styled("— not set", Style::default().fg(WARNING)),
            |path| Span::raw(truncate_start(&terminal_safe(path), 48)),
        );
        let check = project.check_command.as_ref().map_or_else(
            || Span::styled("— not set", Style::default().fg(WARNING)),
            |command| Span::raw(truncate_end(&terminal_safe(&command.join(" ")), 30)),
        );
        // The default limit is muted so a project-specific one stands out.
        let limit = Span::styled(
            format!("{} min", project.check_timeout().as_secs() / 60),
            if project.check_timeout_minutes.is_some() {
                Style::default()
            } else {
                Style::default().fg(MUTED)
            },
        );
        Row::new(vec![
            Cell::from(Line::from(vec![
                Span::styled(
                    terminal_safe(&project.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    project
                        .description
                        .as_deref()
                        .map(|description| format!("  {}", terminal_safe(description)))
                        .unwrap_or_default(),
                    Style::default().fg(MUTED),
                ),
            ])),
            Cell::from(Span::styled(
                status,
                Style::default().fg(project_status_color(&project.status)),
            )),
            Cell::from(repository),
            Cell::from(check),
            Cell::from(limit),
            Cell::from(Span::styled(
                project.task_ids.len().to_string(),
                Style::default().fg(MUTED),
            )),
        ])
    });
    let mut state = TableState::default().with_selected(Some(app.selected_project));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Fill(1),
                Constraint::Length(11),
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Length(7),
                Constraint::Length(5),
            ],
        )
        .header(table_header(&[
            "Name",
            "Status",
            "Repository",
            "Check command",
            "Limit",
            "Tasks",
        ]))
        .column_spacing(2)
        .block(block)
        .highlight_symbol("▌")
        .row_highlight_style(selected_row_style()),
        area,
        &mut state,
    );
}

fn draw_tasks(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let project_name = app.selected_project().map_or_else(
        || "Tasks".to_owned(),
        |project| terminal_safe(&project.name),
    );
    let mut counts = Vec::new();
    for status in [
        TaskStatus::InProgress,
        TaskStatus::Error,
        TaskStatus::Pending,
        TaskStatus::Completed,
    ] {
        let count = app
            .tasks
            .iter()
            .filter(|task| task.status == status)
            .count();
        if count > 0 {
            counts.push(Span::styled(
                format!(" {count} {} ", task_status_label(&status)),
                Style::default().fg(task_status_color(&status)),
            ));
        }
    }
    let block =
        panel(format!("{project_name} · tasks")).title_top(Line::from(counts).right_aligned());
    if app.tasks.is_empty() {
        draw_empty_state(
            frame,
            block,
            area,
            key_hint("No tasks yet. Press ", "n", " to add one."),
        );
        return;
    }
    let rows = app.tasks.iter().map(|task| {
        let step = task.current_step.as_deref().unwrap_or("—");
        let elapsed = (task.status == TaskStatus::InProgress)
            .then(|| step_elapsed(task))
            .flatten()
            .map(elapsed_label)
            .unwrap_or_default();
        let dependencies = match task.dependencies.len() {
            0 => String::new(),
            1 => "1 dep".to_owned(),
            count => format!("{count} deps"),
        };
        Row::new(vec![
            Cell::from(status_badge(&task.status)),
            Cell::from(priority_badge(&task.priority)),
            Cell::from(terminal_safe(&task.title)),
            Cell::from(Span::styled(
                terminal_safe(step),
                Style::default().fg(MUTED),
            )),
            Cell::from(elapsed),
            Cell::from(Span::styled(dependencies, Style::default().fg(MUTED))),
        ])
    });
    let mut state = TableState::default().with_selected(Some(app.selected_task));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(13),
                Constraint::Length(8),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(8),
                Constraint::Length(6),
            ],
        )
        .header(table_header(&[
            "Status", "Priority", "Title", "Step", "Running", "Deps",
        ]))
        .column_spacing(2)
        .block(block)
        .highlight_symbol("▌")
        .row_highlight_style(selected_row_style()),
        area,
        &mut state,
    );
}

fn status_badge(status: &TaskStatus) -> Span<'static> {
    let symbol = match status {
        TaskStatus::Pending => "○",
        TaskStatus::InProgress => "●",
        TaskStatus::Completed => "✓",
        TaskStatus::Rejected => "⊘",
        TaskStatus::Error => "✕",
    };
    Span::styled(
        format!("{symbol} {}", task_status_label(status)),
        Style::default()
            .fg(task_status_color(status))
            .add_modifier(Modifier::BOLD),
    )
}

fn priority_badge(priority: &Priority) -> Span<'static> {
    let (symbol, style) = match priority {
        Priority::High => ("▲", Style::default().fg(WARNING)),
        Priority::Medium => ("■", Style::default()),
        Priority::Low => ("▼", Style::default().fg(MUTED)),
    };
    Span::styled(format!("{symbol} {}", priority_label(priority)), style)
}

/// Seconds the task has spent in its current step.
fn step_elapsed(task: &Task) -> Option<u64> {
    let started = task
        .current_step
        .as_deref()
        .and_then(|step| task.step_started_at_unix_secs.get(step))?;
    Some(now_unix_secs().saturating_sub(*started))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// One line showing every workflow step: done, current, or still ahead.
fn workflow_progress_line(task: &Task) -> Line<'static> {
    let steps = std::iter::successors(Some(WorkflowStep::SpecificationGeneration), |step| {
        step.next()
    })
    .collect::<Vec<_>>();
    let current = task
        .current_step
        .as_deref()
        .and_then(WorkflowStep::parse)
        .and_then(|step| steps.iter().position(|candidate| *candidate == step));
    let mut spans = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ─ ", Style::default().fg(MUTED)));
        }
        let label = match step {
            WorkflowStep::SpecificationGeneration => "specs",
            WorkflowStep::SpecificationApproval => "approve specs",
            WorkflowStep::CodeGeneration => "code",
            WorkflowStep::CodeReview => "review",
            WorkflowStep::Verification => "verify",
            WorkflowStep::FinalApproval => "final approval",
            WorkflowStep::Documentation => "docs",
            WorkflowStep::LocalMerge => "merge",
        };
        let done = task.status == TaskStatus::Completed || current.is_some_and(|at| index < at);
        let (symbol, style) = if done {
            ("✓", Style::default().fg(SUCCESS))
        } else if current == Some(index) {
            let color = match task.status {
                TaskStatus::Error => ERROR,
                TaskStatus::Rejected => MUTED,
                _ if step.requires_human_approval() => WARNING,
                _ => PRIMARY,
            };
            ("●", Style::default().fg(color).add_modifier(Modifier::BOLD))
        } else {
            ("○", Style::default().fg(MUTED))
        };
        spans.push(Span::styled(format!("{symbol} {label}"), style));
    }
    Line::from(spans)
}

fn field(label: &'static str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label}: "),
        Style::default().fg(MUTED),
    )];
    spans.extend(value);
    Line::from(spans)
}

fn draw_task_detail(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let Some(task) = app.selected_task() else {
        draw_empty_state(frame, panel("Task"), area, Line::from("No task selected."));
        return;
    };
    let mut decisions = task
        .recovery_history
        .iter()
        .rev()
        .take(3)
        .map(recovery_record_line)
        .collect::<Vec<_>>();
    decisions.extend(
        task.approval_history
            .iter()
            .rev()
            .take(3)
            .map(approval_record_line),
    );
    if let Some(note) = task.abandonment_note.as_deref() {
        decisions.insert(
            0,
            Line::from(vec![
                Span::styled("Abandoned: ", Style::default().fg(WARNING)),
                Span::raw(terminal_safe(note)),
            ]),
        );
    }
    if decisions.is_empty() {
        decisions.push(Line::styled(
            "No human decisions recorded.",
            Style::default().fg(MUTED),
        ));
    }
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11),
            Constraint::Length(decisions.len().min(6) as u16 + 2),
            Constraint::Min(4),
        ])
        .split(area);

    let mut current_step = terminal_safe(task.current_step.as_deref().unwrap_or("Not started"));
    if task.status == TaskStatus::InProgress
        && let Some(elapsed) = step_elapsed(task)
    {
        current_step.push_str(&format!(" (running {})", elapsed_label(elapsed)));
    }
    let overview = panel(terminal_safe(&task.title)).padding(Padding::horizontal(1));
    let inner = overview.inner(areas[0]);
    frame.render_widget(overview, areas[0]);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Min(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(workflow_progress_line(task)), rows[0]);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .spacing(2)
        .split(rows[2]);
    let left = vec![
        field("Status", vec![status_badge(&task.status)]),
        field("Priority", vec![priority_badge(&task.priority)]),
        field("Active step", vec![Span::raw(current_step)]),
        field(
            "Attempt",
            vec![Span::raw(format!(
                "{}  ·  Automatic revisions: {}/{MAX_AUTOMATIC_REVISIONS}",
                task.attempt, task.automatic_revisions
            ))],
        ),
    ];
    let path_width = usize::from(columns[1].width).saturating_sub(12).max(16);
    let right = vec![
        field(
            "Worktree",
            vec![Span::raw(
                task.worktree
                    .as_deref()
                    .map_or("not created".to_owned(), |path| {
                        truncate_start(&terminal_safe(path), path_width)
                    }),
            )],
        ),
        field(
            "Branch",
            vec![Span::raw(crate::integrations::GitAdapter::worktree_branch(
                task.id,
                task.attempt,
            ))],
        ),
        field("OpenSpec change", vec![Span::raw(task.openspec_change())]),
        field(
            "Approvals",
            vec![
                Span::raw("specs "),
                Span::styled(
                    approval_status(task, ApprovalGate::Specifications),
                    Style::default().fg(approval_status_color(task, ApprovalGate::Specifications)),
                ),
                Span::styled("  ·  ", Style::default().fg(MUTED)),
                Span::raw("final code "),
                Span::styled(
                    approval_status(task, ApprovalGate::FinalCode),
                    Style::default().fg(approval_status_color(task, ApprovalGate::FinalCode)),
                ),
            ],
        ),
    ];
    frame.render_widget(Paragraph::new(left), columns[0]);
    frame.render_widget(Paragraph::new(right), columns[1]);
    frame.render_widget(
        Paragraph::new(match task.description.as_deref() {
            Some(description) => Line::raw(terminal_safe(description)),
            None => Line::styled("No description.", Style::default().fg(MUTED)),
        })
        .wrap(Wrap { trim: true }),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(Text::from(decisions))
            .wrap(Wrap { trim: true })
            .block(panel("Human decisions and revision notes").padding(Padding::horizontal(1))),
        areas[1],
    );

    let block = panel("Agent activity")
        .title_top(Line::styled(" newest last ", Style::default().fg(MUTED)).right_aligned());
    if task.agent_activity.is_empty() {
        draw_empty_state(
            frame,
            block,
            areas[2],
            Line::styled(
                "No agent activity recorded yet.",
                Style::default().fg(MUTED),
            ),
        );
        return;
    }
    let now = now_unix_secs();
    let rows = task
        .agent_activity
        .iter()
        .map(|activity| agent_activity_row(activity, now));
    let mut state = TableState::default().with_selected(Some(app.selected_activity));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Length(3),
                Constraint::Length(14),
                Constraint::Length(25),
                Constraint::Length(17),
                Constraint::Fill(1),
            ],
        )
        .header(table_header(&[
            "When", "#", "Agent", "Step", "Status", "Detail",
        ]))
        .column_spacing(1)
        .block(block)
        .highlight_symbol("▌")
        .row_highlight_style(selected_row_style()),
        areas[2],
        &mut state,
    );
}

fn draw_review(frame: &mut Frame, app: &Dashboard, area: Rect) {
    let title = app.selected_task().map_or("Review".to_owned(), |task| {
        format!("Review · {}", terminal_safe(&task.title))
    });
    let total = app.review_text.len();
    let position = format!(
        " line {}/{} ",
        (usize::from(app.review_scroll) + 1).min(total),
        total
    );
    frame.render_widget(
        Paragraph::new(
            app.review_text
                .iter()
                .map(|line| review_line(line))
                .collect::<Vec<_>>(),
        )
        .scroll((app.review_scroll, 0))
        .block(
            panel(title)
                .title_top(Line::styled(position, Style::default().fg(MUTED)).right_aligned()),
        ),
        area,
    );
}

/// Colors review sections, warnings, and `git diff --stat` lines.
fn review_line(line: &str) -> Line<'static> {
    if let Some(section) = line
        .strip_prefix("== ")
        .and_then(|rest| rest.strip_suffix(" =="))
    {
        return Line::styled(
            format!("▍{section}"),
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        );
    }
    if line.starts_with("WARNING") {
        return Line::styled(
            line.to_owned(),
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        );
    }
    if let Some((file, change)) = line.rsplit_once(" | ")
        && change
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_digit())
    {
        let mut spans = vec![Span::raw(format!("{file} | "))];
        for ch in change.chars() {
            let style = match ch {
                '+' => Style::default().fg(SUCCESS),
                '-' => Style::default().fg(ERROR),
                _ => Style::default(),
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        return Line::from(spans);
    }
    if let Some((label, value)) = line.split_once(": ")
        && matches!(label, "Worktree" | "OpenSpec change")
    {
        return Line::from(vec![
            Span::styled(format!("{label}: "), Style::default().fg(MUTED)),
            Span::raw(value.to_owned()),
        ]);
    }
    Line::raw(line.to_owned())
}

fn truncate_start(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars || max_chars < 2 {
        return value.to_owned();
    }
    let tail = value
        .chars()
        .skip(count - (max_chars - 1))
        .collect::<String>();
    format!("…{tail}")
}

fn truncate_end(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars || max_chars < 2 {
        return value.to_owned();
    }
    let head = value.chars().take(max_chars - 1).collect::<String>();
    format!("{head}…")
}

/// Short relative age such as `just now`, `45s ago`, or `3h ago`.
fn age_label(seconds: u64) -> String {
    match seconds {
        0..5 => "just now".to_owned(),
        5..60 => format!("{seconds}s ago"),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// A labelled form input: the focused field shows a marker, a highlighted label, and a cursor;
/// an empty field shows its placeholder.
fn form_field(focused: bool, label: &str, value: &str, placeholder: &str) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            if focused { "› " } else { "  " },
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{label:<16}"),
            if focused {
                Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED)
            },
        ),
        Span::raw(terminal_safe(value)),
    ];
    if focused {
        spans.push(Span::styled("█", Style::default().fg(PRIMARY)));
    }
    if value.is_empty() {
        spans.push(Span::styled(
            placeholder.to_owned(),
            Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
        ));
    }
    Line::from(spans)
}

fn note_dialog(prompt: &str, note: &str) -> Vec<Line<'static>> {
    vec![
        Line::raw(prompt.to_owned()),
        Line::default(),
        Line::from(vec![
            Span::styled(
                "› ",
                Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
            ),
            Span::raw(terminal_safe(note)),
            Span::styled("█", Style::default().fg(PRIMARY)),
        ]),
    ]
}

fn draw_dialog(frame: &mut Frame, app: &Dashboard) {
    let (title, border, mut lines, hints) = match &app.dialog {
        Dialog::None => return,
        Dialog::DeleteProfile { id } => (
            "Delete agent profile".to_owned(),
            ERROR,
            vec![
                Line::from(vec![
                    Span::raw("Delete profile "),
                    Span::styled(
                        terminal_safe(id),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("?"),
                ]),
                Line::styled(
                    "Agents can no longer select it. Agents already running keep their resources.",
                    Style::default().fg(MUTED),
                ),
            ],
            "y delete  n cancel",
        ),
        Dialog::CreateProject(form) => (
            "Create project".to_owned(),
            PRIMARY,
            vec![
                form_field(
                    form.focus == ProjectField::Name,
                    "Name",
                    &form.name,
                    "required",
                ),
                form_field(
                    form.focus == ProjectField::Description,
                    "Description",
                    &form.description,
                    "optional",
                ),
                form_field(
                    form.focus == ProjectField::Repository,
                    "Git repository",
                    &form.repository,
                    "/path/to/repository",
                ),
                form_field(
                    form.focus == ProjectField::CheckCommand,
                    "Check command",
                    &form.check_command,
                    "./scripts/check.sh (split on spaces, no shell)",
                ),
                form_field(
                    form.focus == ProjectField::CheckTimeout,
                    "Check limit",
                    &form.check_timeout,
                    "minutes, default 60",
                ),
            ],
            "Tab/Shift+Tab fields  Enter create  Esc cancel",
        ),
        Dialog::CreateTask(form) => {
            let priority_focused = form.focus == TaskField::Priority;
            let mut priority = vec![
                Span::styled(
                    if priority_focused { "› " } else { "  " },
                    Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<16}", "Priority"),
                    if priority_focused {
                        Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    },
                ),
            ];
            if priority_focused {
                priority.push(Span::styled("◀ ", Style::default().fg(SHORTCUT)));
            }
            priority.push(priority_badge(&form.priority));
            if priority_focused {
                priority.push(Span::styled(" ▶", Style::default().fg(SHORTCUT)));
            }
            let dependencies_focused = form.focus == TaskField::Dependencies;
            let mut lines = vec![
                form_field(
                    form.focus == TaskField::Title,
                    "Title",
                    &form.title,
                    "required",
                ),
                form_field(
                    form.focus == TaskField::Description,
                    "Description",
                    &form.description,
                    "optional",
                ),
                Line::from(priority),
                form_field(
                    dependencies_focused,
                    "Dependencies",
                    "",
                    "Space toggles the highlighted task",
                ),
            ];
            if app.tasks.is_empty() {
                lines.push(Line::styled(
                    "                  No existing tasks to depend on.",
                    Style::default().fg(MUTED),
                ));
            }
            for (index, task) in app.tasks.iter().enumerate() {
                let cursor = dependencies_focused && form.dependency_cursor == index;
                let checked = form.dependencies.contains(&task.id);
                lines.push(Line::from(vec![
                    Span::raw("                "),
                    Span::styled(
                        if cursor { "› " } else { "  " },
                        Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if checked { "■ " } else { "□ " },
                        Style::default().fg(if checked { PRIMARY } else { MUTED }),
                    ),
                    Span::styled(
                        terminal_safe(&task.title),
                        if cursor {
                            Style::default().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                ]));
            }
            (
                "Create task".to_owned(),
                PRIMARY,
                lines,
                "Tab/Shift+Tab fields  ←/→ priority  ↑/↓ dependency  Enter create  Esc cancel",
            )
        }
        Dialog::Revision { gate, note } => (
            format!("Request changes · {}", approval_gate_label(*gate)),
            WARNING,
            note_dialog("Describe the revision needed:", note),
            "Enter save  Esc cancel",
        ),
        Dialog::Clarify { note } => (
            "Clarify task".to_owned(),
            WARNING,
            note_dialog(
                "This discards the failed worktree. Add context for replacement specifications:",
                note,
            ),
            "Enter save  Esc cancel",
        ),
        Dialog::Abandon { note } => (
            "Abandon task".to_owned(),
            ERROR,
            note_dialog("This marks the task Rejected. Enter the reason:", note),
            "Enter save  Esc cancel",
        ),
    };
    lines.push(Line::default());
    lines.push(shortcut_help_line(hints));

    // Size the dialog to its content: wide enough for the form, tall enough for wrapped lines.
    let screen = frame.area();
    let width = 84.min(screen.width.saturating_sub(4)).max(20);
    let inner_width = usize::from(width.saturating_sub(4)).max(1);
    let content_height = lines
        .iter()
        .map(|line| line.width().div_ceil(inner_width).max(1))
        .sum::<usize>();
    let height = (content_height as u16 + 3).min(screen.height.saturating_sub(2));
    let area = Rect {
        x: screen.x + screen.width.saturating_sub(width) / 2,
        y: screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border))
                .padding(Padding::new(1, 1, 1, 0))
                .title(Span::styled(
                    format!(" {title} "),
                    Style::default().fg(border).add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

fn elapsed_label(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

fn recovery_record_line(record: &crate::model::RecoveryRecord) -> Line<'static> {
    let action = match record.action {
        crate::model::RecoveryAction::Retry => "retry",
        crate::model::RecoveryAction::Clarify => "clarification",
        crate::model::RecoveryAction::RetryStep => "step retry",
    };
    Line::from(format!(
        "Attempt {} — {action}: {}",
        record.attempt,
        terminal_safe(record.note.as_deref().unwrap_or("")),
    ))
}

fn approval_record_line(record: &crate::model::ApprovalRecord) -> Line<'static> {
    let note = record.note.as_deref().unwrap_or("");
    Line::from(vec![
        Span::raw(format!(
            "Attempt {} — {}: ",
            record.attempt,
            approval_gate_label(record.gate),
        )),
        Span::styled(
            approval_decision_label(record.decision),
            Style::default().fg(approval_decision_color(record.decision)),
        ),
        Span::raw(format!("  {}", terminal_safe(note))),
    ])
}

fn approval_gate_label(gate: ApprovalGate) -> &'static str {
    match gate {
        ApprovalGate::Specifications => "specifications",
        ApprovalGate::FinalCode => "final code",
    }
}

fn approval_decision_label(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approved => "approved",
        ApprovalDecision::Rejected => "revision requested",
    }
}

fn project_status_color(status: &ProjectStatus) -> Color {
    match status {
        ProjectStatus::Active => PRIMARY,
        ProjectStatus::Archived => WARNING,
    }
}

fn task_status_color(status: &TaskStatus) -> Color {
    match status {
        TaskStatus::Pending | TaskStatus::Rejected => WARNING,
        TaskStatus::InProgress => PRIMARY,
        TaskStatus::Completed => SUCCESS,
        TaskStatus::Error => ERROR,
    }
}

fn agent_status_color(status: &AgentStatus) -> Color {
    match status {
        AgentStatus::Starting | AgentStatus::Running => PRIMARY,
        AgentStatus::Waiting
        | AgentStatus::Cancelled
        | AgentStatus::Blocked
        | AgentStatus::ChangesRequested => WARNING,
        AgentStatus::Completed => SUCCESS,
        AgentStatus::Failed => ERROR,
    }
}

fn approval_decision_color(decision: ApprovalDecision) -> Color {
    match decision {
        ApprovalDecision::Approved => SUCCESS,
        ApprovalDecision::Rejected => WARNING,
    }
}

fn approval_status(task: &Task, gate: ApprovalGate) -> &'static str {
    task.approval_history
        .iter()
        .rev()
        .find(|record| record.attempt == task.attempt && record.gate == gate)
        .map_or("pending", |record| approval_decision_label(record.decision))
}

fn approval_status_color(task: &Task, gate: ApprovalGate) -> Color {
    task.approval_history
        .iter()
        .rev()
        .find(|record| record.attempt == task.attempt && record.gate == gate)
        .map_or(WARNING, |record| approval_decision_color(record.decision))
}

fn shortcut_help_line(hints: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, hint) in hints.split("  ").enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        if let Some((shortcut, description)) = hint.split_once(' ') {
            spans.push(Span::styled(
                shortcut.to_owned(),
                Style::default().fg(SHORTCUT).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" {description}"),
                Style::default().fg(MUTED),
            ));
        } else {
            spans.push(Span::styled(
                hint.to_owned(),
                Style::default().fg(SHORTCUT).add_modifier(Modifier::BOLD),
            ));
        }
    }
    Line::from(spans)
}

fn agent_activity_row(activity: &AgentActivity, now: u64) -> Row<'static> {
    Row::new(vec![
        Cell::from(Span::styled(
            age_label(now.saturating_sub(activity.updated_at_unix_secs)),
            Style::default().fg(MUTED),
        )),
        Cell::from(Span::styled(
            activity.attempt.to_string(),
            Style::default().fg(MUTED),
        )),
        Cell::from(terminal_safe(&activity.agent_name)),
        Cell::from(Span::styled(
            terminal_safe(&activity.step),
            Style::default().fg(MUTED),
        )),
        Cell::from(Span::styled(
            agent_status_label(&activity.status),
            Style::default().fg(agent_status_color(&activity.status)),
        )),
        Cell::from(terminal_safe(activity.detail.as_deref().unwrap_or(""))),
    ])
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Rejected => "rejected",
        TaskStatus::Error => "error",
    }
}

fn priority_label(priority: &Priority) -> &'static str {
    match priority {
        Priority::Low => "low",
        Priority::Medium => "medium",
        Priority::High => "high",
    }
}

fn agent_status_label(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Running => "running",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Completed => "completed",
        AgentStatus::ChangesRequested => "changes requested",
        AgentStatus::Failed => "failed",
        AgentStatus::Cancelled => "cancelled",
        AgentStatus::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Dashboard, Dialog, ERROR, PRIMARY, SHORTCUT, SUCCESS, Screen, WARNING, agent_status_color,
        approval_decision_color, project_status_color, task_status_color,
    };
    use crate::{
        model::{
            AgentStatus, ApprovalDecision, ApprovalGate, Priority, ProjectStatus, TaskStatus,
            WorkflowStep,
        },
        project::ProjectManager,
    };
    use crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    fn render(app: &Dashboard) -> TestBackend {
        let backend = TestBackend::new(180, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| super::draw(frame, app)).unwrap();
        terminal.backend().clone()
    }

    fn rendered_text_position(backend: &TestBackend, text: &str) -> (u16, u16) {
        let buffer = backend.buffer();
        for y in 0..buffer.area.height {
            let row = (0..buffer.area.width).fold(String::new(), |mut row, x| {
                row.push_str(buffer.cell((x, y)).unwrap().symbol());
                row
            });
            if let Some(start) = row.find(text) {
                return (row[..start].chars().count() as u16, y);
            }
        }
        panic!("rendered text not found: {text:?}");
    }

    fn assert_rendered_color(backend: &TestBackend, text: &str, expected: Color) {
        let (x, y) = rendered_text_position(backend, text);
        assert_eq!(
            backend.buffer().cell((x, y)).unwrap().fg,
            expected,
            "style for {text:?}"
        );
    }

    #[test]
    fn palette_colors_and_status_roles_match_the_requested_semantics() {
        assert_eq!(PRIMARY, ratatui::style::Color::Rgb(188, 147, 248));
        assert_eq!(WARNING, ratatui::style::Color::Rgb(255, 184, 108));
        assert_eq!(ERROR, ratatui::style::Color::Rgb(254, 86, 84));
        assert_eq!(SHORTCUT, ratatui::style::Color::Rgb(222, 110, 177));
        assert_eq!(SUCCESS, ratatui::style::Color::Rgb(0, 106, 99));

        assert_eq!(project_status_color(&ProjectStatus::Active), PRIMARY);
        assert_eq!(project_status_color(&ProjectStatus::Archived), WARNING);
        assert_eq!(task_status_color(&TaskStatus::Pending), WARNING);
        assert_eq!(task_status_color(&TaskStatus::InProgress), PRIMARY);
        assert_eq!(task_status_color(&TaskStatus::Completed), SUCCESS);
        assert_eq!(task_status_color(&TaskStatus::Rejected), WARNING);
        assert_eq!(task_status_color(&TaskStatus::Error), ERROR);
        assert_eq!(agent_status_color(&AgentStatus::Waiting), WARNING);
        assert_eq!(agent_status_color(&AgentStatus::Failed), ERROR);
        assert_eq!(agent_status_color(&AgentStatus::Completed), SUCCESS);
        assert_eq!(approval_decision_color(ApprovalDecision::Approved), SUCCESS);
        assert_eq!(approval_decision_color(ApprovalDecision::Rejected), WARNING);
    }

    #[test]
    fn title_is_at_the_top_in_primary_color_on_every_dashboard_screen() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        manager
            .create_task(
                project.id,
                "Build feature".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);

        for screen in [Screen::Projects, Screen::Tasks, Screen::TaskDetail] {
            app.screen = screen;
            let backend = render(&app);
            let (x, y) = rendered_text_position(&backend, "Cronos-AI");
            assert_eq!(y, 0);
            assert_eq!(backend.buffer().cell((x, y)).unwrap().fg, PRIMARY);
        }
    }

    #[test]
    fn rendered_task_states_errors_and_shortcuts_use_semantic_colors() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        manager
            .create_task(
                project.id,
                "Build feature".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.error = Some("simulated failure".to_owned());

        let backend = render(&app);
        assert_rendered_color(&backend, "pending", WARNING);
        assert_rendered_color(&backend, "n new task", SHORTCUT);
        assert_rendered_color(&backend, "Error: simulated failure", ERROR);

        app.screen = Screen::TaskDetail;
        let task = &mut app.tasks[0];
        task.status = TaskStatus::Completed;
        task.approval_history = vec![
            crate::model::ApprovalRecord {
                attempt: task.attempt,
                gate: ApprovalGate::Specifications,
                decision: ApprovalDecision::Approved,
                note: None,
                decided_at_unix_secs: 1,
            },
            crate::model::ApprovalRecord {
                attempt: task.attempt,
                gate: ApprovalGate::FinalCode,
                decision: ApprovalDecision::Rejected,
                note: Some("Revise this change".to_owned()),
                decided_at_unix_secs: 2,
            },
        ];
        task.agent_activity.push(crate::model::AgentActivity {
            attempt: task.attempt,
            agent_name: "reviewer".to_owned(),
            step: "code-review".to_owned(),
            status: AgentStatus::Failed,
            detail: Some("test failure".to_owned()),
            updated_at_unix_secs: 3,
        });

        let backend = render(&app);
        assert_rendered_color(&backend, "completed", SUCCESS);
        assert_rendered_color(&backend, "approved", SUCCESS);
        assert_rendered_color(&backend, "revision requested", WARNING);
        assert_rendered_color(&backend, "failed", ERROR);

        app.dialog = Dialog::Abandon {
            note: String::new(),
        };
        let backend = render(&app);
        assert_rendered_color(&backend, "Enter save", SHORTCUT);
        assert_rendered_color(&backend, "Esc cancel", SHORTCUT);
    }

    #[test]
    fn rendered_user_content_cannot_inject_terminal_control_sequences() {
        let rendered = super::terminal_safe("task\u{1b}[2J\nname");
        assert_eq!(rendered, "task [2J name");
        assert!(!rendered.chars().any(char::is_control));
    }

    #[test]
    fn project_creation_saves_and_selects_project_from_form() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();

        app.handle_key(KeyCode::Char('n'));
        for character in "Factory".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Tab);
        for character in "Created in TUI".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);

        let projects = ProjectManager::new(root.path())
            .unwrap()
            .list_projects()
            .unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Factory");
        assert_eq!(projects[0].description.as_deref(), Some("Created in TUI"));
        assert_eq!(projects[0].status, ProjectStatus::Active);
        assert_eq!(app.selected_project().unwrap().id, projects[0].id);
        assert!(matches!(app.dialog, Dialog::None));
    }

    #[test]
    fn project_form_saves_check_command_as_separate_arguments() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Char('n'));
        for character in "Factory".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        for _ in 0..3 {
            app.handle_key(KeyCode::Tab);
        }
        let backend = render(&app);
        assert!(rendered_text_position(&backend, "› Check command").1 > 0);
        for character in "./scripts/check.sh  --all".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let project = &ProjectManager::new(root.path())
            .unwrap()
            .list_projects()
            .unwrap()[0];
        assert_eq!(
            project.check_command.as_deref(),
            Some(&["./scripts/check.sh".to_owned(), "--all".to_owned()][..])
        );
    }

    #[test]
    fn project_list_shows_a_custom_check_time_limit() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Timed".to_owned(), None).unwrap();
        manager
            .set_project_check_command(project.id, vec!["make".to_owned(), "check".to_owned()])
            .unwrap();
        manager
            .set_project_check_timeout(project.id, Some(90))
            .unwrap();
        let app = Dashboard::new(manager).unwrap();

        let backend = render(&app);
        assert!(rendered_text_position(&backend, "90 min").1 > 0);
    }

    #[test]
    fn project_form_saves_a_check_time_limit_and_rejects_invalid_minutes() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Char('n'));
        for character in "Factory".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        }
        let backend = render(&app);
        assert!(rendered_text_position(&backend, "› Check limit").1 > 0);
        assert!(rendered_text_position(&backend, "minutes, default 60").1 > 0);

        for character in "abc".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        assert!(matches!(app.dialog, Dialog::CreateProject(_)));
        assert!(
            app.error
                .as_deref()
                .unwrap()
                .contains("whole number of minutes")
        );
        assert!(app.manager.list_projects().unwrap().is_empty());

        for _ in 0..3 {
            app.handle_key(KeyCode::Backspace);
        }
        for character in "90".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        assert!(matches!(app.dialog, Dialog::None));
        let project = &app.manager.list_projects().unwrap()[0];
        assert_eq!(project.check_timeout_minutes, Some(90));
    }

    #[test]
    fn task_details_show_location_progress_and_step_retry() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);
        let backend = render(&app);
        rendered_text_position(&backend, "specification-generation (running 0m");
        rendered_text_position(&backend, "Worktree: not created");
        rendered_text_position(
            &backend,
            &format!("OpenSpec change: {}", task.openspec_change()),
        );
        rendered_text_position(&backend, "Automatic revisions: 0/3");

        app.handle_key(KeyCode::Char('v'));
        assert_eq!(app.screen, Screen::TaskDetail);
        assert!(app.error.as_deref().unwrap().contains("no Git repository"));

        manager
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        app.refresh_for_ui();
        let backend = render(&app);
        assert_rendered_color(&backend, "t retry step", SHORTCUT);
        app.handle_key(KeyCode::Char('t'));
        let resumed = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(resumed.status, TaskStatus::InProgress, "{:?}", app.error);
        assert_eq!(resumed.attempt, 1);
        assert_eq!(
            resumed.recovery_history.last().unwrap().action,
            crate::model::RecoveryAction::RetryStep
        );
    }

    #[test]
    fn review_view_shows_task_branch_commits_and_scrolls() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(repository.join("openspec")).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repository, &["init"]);
        git(&repository, &["config", "user.name", "Cronos Tests"]);
        git(
            &repository,
            &["config", "user.email", "cronos@example.invalid"],
        );
        std::fs::write(
            repository.join("openspec/config.yaml"),
            "schema: spec-driven\n",
        )
        .unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-m", "initial"]);
        let manager = ProjectManager::new(temp.path().join("data")).unwrap();
        let project = manager
            .create_project_with_repository(
                "Demo".to_owned(),
                None,
                Some(&repository),
                None,
                None,
                &crate::integrations::GitAdapter::default(),
            )
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let worktree = manager
            .prepare_task_worktree(
                project.id,
                task.id,
                &crate::integrations::GitAdapter::default(),
            )
            .unwrap();
        std::fs::write(worktree.join("feature.txt"), "new\n").unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-m", "feat: add the feature"]);

        let mut app =
            Dashboard::new(ProjectManager::new(temp.path().join("data")).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('v'));
        assert_eq!(app.screen, Screen::Review);
        let text = app.review_text.join("\n");
        assert!(text.contains("feat: add the feature"));
        assert!(text.contains("feature.txt"));
        assert!(text.contains("check command has not run"));
        let backend = render(&app);
        rendered_text_position(&backend, "Review · Build");
        app.handle_key(KeyCode::Down);
        assert_eq!(app.review_scroll, 1);
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.screen, Screen::TaskDetail);
    }

    #[test]
    fn invalid_project_repository_keeps_form_values_and_does_not_save() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();

        app.handle_key(KeyCode::Char('n'));
        for character in "Factory".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        for character in "/definitely/not/a/git/repository".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);

        assert!(app.error.is_some());
        match &app.dialog {
            Dialog::CreateProject(form) => {
                assert_eq!(form.name, "Factory");
                assert_eq!(form.repository, "/definitely/not/a/git/repository");
            }
            _ => panic!("invalid submission should leave the project form open"),
        }
        assert!(
            ProjectManager::new(root.path())
                .unwrap()
                .list_projects()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cancelling_project_creation_discards_form_without_saving() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();

        app.handle_key(KeyCode::Char('n'));
        for character in "Do not save".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Esc);

        assert!(matches!(app.dialog, Dialog::None));
        assert!(
            ProjectManager::new(root.path())
                .unwrap()
                .list_projects()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn task_creation_saves_selected_fields_and_dependency_without_starting_work() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let prerequisite = manager
            .create_task(
                project.id,
                "Prerequisite".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('n'));
        for character in "Implement the next step".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Tab);
        for character in "Created through the TUI".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char(' '));
        app.handle_key(KeyCode::Enter);

        let task = ProjectManager::new(root.path())
            .unwrap()
            .list_tasks(project.id)
            .unwrap()
            .into_iter()
            .find(|task| task.title == "Implement the next step")
            .unwrap();
        assert_eq!(task.description.as_deref(), Some("Created through the TUI"));
        assert_eq!(task.priority, Priority::High);
        assert_eq!(task.dependencies, vec![prerequisite.id]);
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.current_step.is_none());
        assert!(task.worktree.is_none());
        assert_eq!(app.selected_task().unwrap().id, task.id);
        assert!(matches!(app.dialog, Dialog::None));
    }

    #[test]
    fn task_creation_defaults_to_medium_priority_without_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('n'));
        for character in "Independent task".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);

        let task = manager.list_tasks(project.id).unwrap().pop().unwrap();
        assert_eq!(task.priority, Priority::Medium);
        assert!(task.dependencies.is_empty());
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[test]
    fn invalid_task_title_keeps_form_and_does_not_save() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('n'));
        app.handle_key(KeyCode::Enter);

        assert!(app.error.is_some());
        match &app.dialog {
            Dialog::CreateTask(form) => assert!(form.title.is_empty()),
            _ => panic!("invalid submission should leave the task form open"),
        }
        assert!(manager.list_tasks(project.id).unwrap().is_empty());
    }

    #[test]
    fn cancelling_task_creation_discards_form_without_saving() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('n'));
        for character in "Do not save".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Esc);

        assert!(matches!(app.dialog, Dialog::None));
        assert!(manager.list_tasks(project.id).unwrap().is_empty());
    }

    #[test]
    fn archived_project_rejects_task_creation_from_tui() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Archived".to_owned(), None).unwrap();
        manager.archive_project(project.id).unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('n'));

        assert!(
            app.error
                .as_deref()
                .is_some_and(|error| error.contains("Archived"))
        );
        assert!(matches!(app.dialog, Dialog::None));
        assert!(manager.list_tasks(project.id).unwrap().is_empty());
    }

    #[test]
    fn project_and_dependent_task_creation_roundtrips_through_tui_controller() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();

        app.handle_key(KeyCode::Char('n'));
        for character in "E2E project".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);

        app.handle_key(KeyCode::Char('n'));
        for character in "Prerequisite".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.list_projects().unwrap().pop().unwrap();
        let prerequisite = manager.list_tasks(project.id).unwrap().pop().unwrap();

        app.handle_key(KeyCode::Char('n'));
        for character in "Dependent task".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char(' '));
        app.handle_key(KeyCode::Enter);

        let reloaded = ProjectManager::new(root.path()).unwrap();
        let tasks = reloaded.list_tasks(project.id).unwrap();
        let dependent = tasks
            .iter()
            .find(|task| task.title == "Dependent task")
            .unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(dependent.dependencies, vec![prerequisite.id]);
        assert_eq!(dependent.priority, Priority::Medium);
        assert_eq!(dependent.status, TaskStatus::Pending);
        assert!(dependent.current_step.is_none());
        assert!(dependent.worktree.is_none());
        assert_eq!(app.selected_task().unwrap().id, dependent.id);
    }

    #[test]
    fn navigation_opens_project_tasks_and_agent_activity_then_returns() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Implement TUI".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager
            .set_task_status(project.id, task.id, TaskStatus::InProgress)
            .unwrap();
        manager
            .record_agent_activity(
                project.id,
                task.id,
                "builder".to_owned(),
                "implementation".to_owned(),
                AgentStatus::Running,
                Some("editing files".to_owned()),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();

        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::Tasks);
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::TaskDetail);
        assert_eq!(app.selected_task().unwrap().agent_activity.len(), 1);
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.screen, Screen::Tasks);
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.screen, Screen::Projects);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn approval_controls_record_revision_notes_and_keep_task_in_progress() {
        let root = tempfile::tempdir().unwrap();
        let writer = ProjectManager::new(root.path()).unwrap();
        let project = writer.create_project("Demo".to_owned(), None).unwrap();
        let task = writer
            .create_task(
                project.id,
                "Approve specs".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        writer.start_next_task(project.id).unwrap().unwrap();
        writer
            .complete_workflow_step(project.id, task.id, WorkflowStep::SpecificationGeneration)
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);

        app.handle_key(KeyCode::Char('n'));
        assert!(matches!(&app.dialog, Dialog::Revision { .. }));
        for character in "Add acceptance criteria".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let revised = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(revised.status, TaskStatus::InProgress);
        assert_eq!(
            revised.current_step.as_deref(),
            Some("specification-generation")
        );
        assert_eq!(
            revised.approval_history[0].decision,
            ApprovalDecision::Rejected
        );
        assert_eq!(
            revised.approval_history[0].note.as_deref(),
            Some("Add acceptance criteria")
        );

        writer
            .complete_workflow_step(project.id, task.id, WorkflowStep::SpecificationGeneration)
            .unwrap();
        app.refresh().unwrap();
        app.handle_key(KeyCode::Char('y'));
        let approved = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(approved.current_step.as_deref(), Some("code-generation"));
        assert!(approved.is_approved(ApprovalGate::Specifications));

        for step in [
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
        ] {
            writer
                .complete_workflow_step(project.id, task.id, step)
                .unwrap();
        }
        app.refresh().unwrap();
        app.handle_key(KeyCode::Char('n'));
        for character in "Fix the failing test".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let revised = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(revised.status, TaskStatus::InProgress);
        assert_eq!(revised.current_step.as_deref(), Some("code-generation"));
        assert_eq!(
            revised.approval_history.last().unwrap().gate,
            ApprovalGate::FinalCode
        );
        assert_eq!(
            revised.approval_history.last().unwrap().decision,
            ApprovalDecision::Rejected
        );

        for step in [
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
        ] {
            writer
                .complete_workflow_step(project.id, task.id, step)
                .unwrap();
        }
        app.refresh().unwrap();
        app.handle_key(KeyCode::Char('y'));
        let final_approved = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(
            final_approved.current_step.as_deref(),
            Some("documentation")
        );
        assert!(final_approved.is_approved(ApprovalGate::FinalCode));
    }

    #[test]
    fn abandonment_dialog_requires_a_reason_and_marks_task_rejected() {
        let root = tempfile::tempdir().unwrap();
        let writer = ProjectManager::new(root.path()).unwrap();
        let project = writer.create_project("Demo".to_owned(), None).unwrap();
        let task = writer
            .create_task(
                project.id,
                "Abandon task".to_owned(),
                None,
                Priority::Low,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('a'));
        app.handle_key(KeyCode::Enter);
        assert!(matches!(&app.dialog, Dialog::Abandon { .. }));
        for character in "No longer needed".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let rejected = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(rejected.status, TaskStatus::Rejected);
        assert_eq!(
            rejected.abandonment_note.as_deref(),
            Some("No longer needed")
        );
    }

    #[test]
    fn error_task_controls_retry_or_save_clarification_context() {
        let root = tempfile::tempdir().unwrap();
        let writer = ProjectManager::new(root.path()).unwrap();
        let project = writer.create_project("Demo".to_owned(), None).unwrap();
        let task = writer
            .create_task(
                project.id,
                "Recover me".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        writer.start_next_task(project.id).unwrap();
        writer
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Enter);

        app.handle_key(KeyCode::Char('r'));
        let retried = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(retried.status, TaskStatus::InProgress);
        assert_eq!(retried.attempt, 2);
        assert_eq!(
            retried.current_step.as_deref(),
            Some("specification-generation")
        );

        writer
            .set_task_status(project.id, task.id, TaskStatus::Error)
            .unwrap();
        app.refresh().unwrap();
        app.handle_key(KeyCode::Char('c'));
        for character in "Handle no network access".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        let clarified = writer.get_task(project.id, task.id).unwrap();
        assert_eq!(clarified.status, TaskStatus::InProgress);
        assert_eq!(clarified.attempt, 3);
        assert_eq!(
            clarified.current_step.as_deref(),
            Some("specification-generation")
        );
        assert_eq!(
            clarified.recovery_history.last().unwrap().note.as_deref(),
            Some("Handle no network access")
        );
    }

    #[test]
    fn starting_the_queue_does_not_claim_a_task_without_a_configured_repository() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Needs worktree".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('s'));
        assert_eq!(
            manager.get_task(project.id, task.id).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn refresh_reads_workflow_updates_from_the_shared_store() {
        let root = tempfile::tempdir().unwrap();
        let writer = ProjectManager::new(root.path()).unwrap();
        let project = writer.create_project("Demo".to_owned(), None).unwrap();
        let task = writer
            .create_task(
                project.id,
                "Watch status".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        writer.start_next_task(project.id).unwrap().unwrap();
        writer
            .record_agent_activity(
                project.id,
                task.id,
                "spec-writer".to_owned(),
                "specification-generation".to_owned(),
                AgentStatus::Running,
                Some("drafting proposal".to_owned()),
            )
            .unwrap();
        app.refresh().unwrap();
        let task = app.selected_task().unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(
            task.current_step.as_deref(),
            Some("specification-generation")
        );
        assert_eq!(task.agent_activity.len(), 1);
        assert_eq!(app.selected_activity, 0);

        writer
            .record_agent_activity(
                project.id,
                task.id,
                "spec-writer".to_owned(),
                "specification-generation".to_owned(),
                AgentStatus::Completed,
                Some("draft ready".to_owned()),
            )
            .unwrap();
        app.refresh().unwrap();
        assert_eq!(app.selected_activity, 1);
    }

    fn git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn openspec_repository(root: &std::path::Path) -> std::path::PathBuf {
        let repository = root.join("repo");
        std::fs::create_dir_all(repository.join("openspec")).unwrap();
        git(&repository, &["init"]);
        git(&repository, &["config", "user.name", "Cronos Test"]);
        git(
            &repository,
            &["config", "user.email", "cronos@example.invalid"],
        );
        std::fs::write(repository.join("openspec/config.yaml"), "schema: x\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-m", "initial"]);
        repository
    }

    #[test]
    fn starting_the_queue_requires_a_check_command() {
        let root = tempfile::tempdir().unwrap();
        let repository = openspec_repository(root.path());
        let manager = ProjectManager::new(root.path().join("data")).unwrap();
        let project = manager
            .create_project_with_repository(
                "Demo".to_owned(),
                None,
                Some(&repository),
                None,
                None,
                &crate::integrations::GitAdapter::default(),
            )
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Needs a check".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        let mut app =
            Dashboard::new(ProjectManager::new(root.path().join("data")).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('s'));
        assert!(app.error.as_deref().unwrap().contains("check command"));
        assert_eq!(
            manager.get_task(project.id, task.id).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn resume_at_local_merge_waits_for_a_clean_base_then_merges() {
        let root = tempfile::tempdir().unwrap();
        let repository = openspec_repository(root.path());
        let data = root.path().join("data");
        let manager = ProjectManager::new(&data).unwrap();
        let git_adapter = crate::integrations::GitAdapter::default();
        let project = manager
            .create_project_with_repository(
                "Demo".to_owned(),
                None,
                Some(&repository),
                Some(vec!["true".to_owned()]),
                None,
                &git_adapter,
            )
            .unwrap();
        let task = manager
            .create_task(
                project.id,
                "Merge me".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap().unwrap();
        let worktree = manager
            .prepare_task_worktree(project.id, task.id, &git_adapter)
            .unwrap();
        std::fs::write(worktree.join("feature.txt"), "done\n").unwrap();
        git(&worktree, &["add", "feature.txt"]);
        git(&worktree, &["commit", "-m", "feat: add feature"]);
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
        for step in [
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
        ] {
            manager
                .complete_workflow_step(project.id, task.id, step)
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
            .complete_workflow_step(project.id, task.id, WorkflowStep::Documentation)
            .unwrap();

        let mut app = Dashboard::new(ProjectManager::new(&data).unwrap()).unwrap();
        app.handle_key(KeyCode::Enter);
        std::fs::write(repository.join("scratch.txt"), "local edit\n").unwrap();
        app.resume_interrupted_task(project.id, task.id);
        assert!(
            app.error
                .as_deref()
                .unwrap()
                .contains("uncommitted changes")
        );
        let waiting = manager.get_task(project.id, task.id).unwrap();
        assert_eq!(waiting.status, TaskStatus::InProgress);
        assert_eq!(waiting.current_step.as_deref(), Some("local-merge"));

        std::fs::remove_file(repository.join("scratch.txt")).unwrap();
        app.resume_interrupted_task(project.id, task.id);
        assert_eq!(
            manager.get_task(project.id, task.id).unwrap().status,
            TaskStatus::Completed
        );
        assert!(repository.join("feature.txt").is_file());
    }

    #[test]
    fn workflow_progress_marks_done_current_and_upcoming_steps() {
        let root = tempfile::tempdir().unwrap();
        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        let mut task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::Medium,
                Vec::new(),
            )
            .unwrap();
        task.status = TaskStatus::InProgress;
        task.current_step = Some("code-review".to_owned());
        let line = super::workflow_progress_line(&task);
        let text = line.to_string();
        assert!(text.starts_with("✓ specs ─ ✓ approve specs ─ ✓ code ─ ● review ─ ○ verify"));
        let span = |label: &str| {
            line.spans
                .iter()
                .find(|span| span.content.ends_with(label))
                .unwrap()
                .style
                .fg
        };
        assert_eq!(span("code"), Some(SUCCESS));
        assert_eq!(span("review"), Some(PRIMARY));
        assert_eq!(span("merge"), Some(super::MUTED));

        task.status = TaskStatus::Error;
        assert_eq!(
            super::workflow_progress_line(&task)
                .spans
                .iter()
                .find(|span| span.content.ends_with("review"))
                .unwrap()
                .style
                .fg,
            Some(ERROR)
        );
        task.status = TaskStatus::Completed;
        assert!(
            !super::workflow_progress_line(&task)
                .to_string()
                .contains('○')
        );
    }

    #[test]
    fn labels_shorten_paths_and_describe_ages() {
        assert_eq!(super::age_label(2), "just now");
        assert_eq!(super::age_label(45), "45s ago");
        assert_eq!(super::age_label(125), "2m ago");
        assert_eq!(super::age_label(7_200), "2h ago");
        assert_eq!(super::age_label(200_000), "2d ago");
        assert_eq!(super::truncate_start("/a/very/long/path", 8), "…ng/path");
        assert_eq!(super::truncate_start("short", 8), "short");
        assert_eq!(super::truncate_end("cargo test --all", 8), "cargo t…");
    }

    #[test]
    fn empty_lists_explain_how_to_add_items_and_selection_keeps_status_colors() {
        let root = tempfile::tempdir().unwrap();
        let mut app = Dashboard::new(ProjectManager::new(root.path()).unwrap()).unwrap();
        rendered_text_position(&render(&app), "No projects yet. Press n to create one.");

        let manager = ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Demo".to_owned(), None).unwrap();
        app.refresh().unwrap();
        app.handle_key(KeyCode::Enter);
        rendered_text_position(&render(&app), "No tasks yet. Press n to add one.");

        manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        app.refresh().unwrap();
        let backend = render(&app);
        // The selected row's status badge keeps its color on the selection background.
        let (x, y) = rendered_text_position(&backend, "○ pending");
        let cell = backend.buffer().cell((x + 2, y)).unwrap();
        assert_eq!(cell.fg, WARNING);
        assert_eq!(cell.bg, super::SELECTION);
        rendered_text_position(&backend, "Projects › Demo");
        rendered_text_position(&backend, "○ orchestrator idle");
    }

    fn type_text(app: &mut Dashboard, text: &str) {
        for character in text.chars() {
            app.handle_key(KeyCode::Char(character));
        }
    }

    #[test]
    fn agent_profiles_are_created_from_the_tui() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("skills/review");
        std::fs::create_dir_all(&skill).unwrap();
        let mut app =
            Dashboard::new(ProjectManager::new(root.path().join("data")).unwrap()).unwrap();
        app.handle_key(KeyCode::Char('p'));
        assert_eq!(app.screen, Screen::Profiles);
        rendered_text_position(&render(&app), "No agent profiles yet.");

        app.handle_key(KeyCode::Char('n'));
        assert_eq!(app.screen, Screen::ProfileEditor);
        type_text(&mut app, "reviewer");
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, "Reviews code for security");
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char(' '));
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, "provider/model");
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, "1x5");
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, &skill.to_string_lossy());
        app.handle_key(KeyCode::Enter);
        // 'q' is text inside the editor, not quit.
        type_text(&mut app, "q");
        app.handle_key(KeyCode::Backspace);
        let backend = render(&app);
        rendered_text_position(&backend, "New agent profile");
        rendered_text_position(&backend, &format!("✓ {}", skill.display()));
        app.handle_key(KeyCode::Enter);

        assert!(!app.should_quit);
        assert_eq!(app.screen, Screen::Profiles, "{:?}", app.error);
        let catalog =
            crate::agent_profiles::AgentProfileCatalog::load(&root.path().join("data")).unwrap();
        let saved = &catalog.profiles()[0];
        assert_eq!(saved.id, "reviewer");
        assert_eq!(saved.steps, vec!["code-review".to_owned()]);
        assert_eq!(saved.skills, vec![skill.clone()]);
        assert_eq!(saved.model.as_deref(), Some("provider/model"));
        assert_eq!(saved.timeout_minutes, Some(15));
        let backend = render(&app);
        rendered_text_position(&backend, "✓ ready");
        rendered_text_position(&backend, "reviewer · resources");
    }

    #[test]
    fn invalid_profiles_keep_the_editor_open_and_save_nothing() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let mut app = Dashboard::new(ProjectManager::new(&data).unwrap()).unwrap();
        app.handle_key(KeyCode::Char('p'));
        app.handle_key(KeyCode::Char('n'));
        type_text(&mut app, "Bad ID");
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::ProfileEditor);
        assert!(app.error.as_deref().unwrap().contains("profile ID"));

        for _ in 0..6 {
            app.handle_key(KeyCode::Backspace);
        }
        type_text(&mut app, "writer");
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, "Writes code");
        app.handle_key(KeyCode::Enter);
        assert!(
            app.error
                .as_deref()
                .unwrap()
                .contains("at least one agent workflow step")
        );

        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char(' '));
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        }
        type_text(&mut app, "/definitely/missing/adapter");
        app.handle_key(KeyCode::Enter);
        assert!(
            app.error
                .as_deref()
                .unwrap()
                .contains("MCP adapter and the MCP config")
        );
        app.handle_key(KeyCode::Tab);
        type_text(&mut app, "/definitely/missing/config.json");
        app.handle_key(KeyCode::Enter);
        assert!(app.error.as_deref().unwrap().contains("missing"));
        assert_eq!(app.screen, Screen::ProfileEditor);
        assert!(!data.join("agent-profiles.json").exists());

        app.handle_key(KeyCode::Esc);
        assert_eq!(app.screen, Screen::Profiles);
        assert!(!data.join("agent-profiles.json").exists());
    }

    #[test]
    fn a_broken_profile_is_shown_fixed_and_deleted_from_the_tui() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let skill = root.path().join("gone");
        std::fs::create_dir_all(&skill).unwrap();
        let profile =
            |id: &str, skills: Vec<std::path::PathBuf>| crate::agent_profiles::AgentProfile {
                id: id.to_owned(),
                description: format!("{id} profile"),
                steps: vec!["code-review".to_owned()],
                skills,
                extensions: Vec::new(),
                mcp: None,
                model: None,
                timeout_minutes: None,
            };
        std::fs::create_dir_all(&data).unwrap();
        crate::agent_profiles::save_profiles(
            &data,
            vec![
                profile("broken", vec![skill.clone()]),
                profile("docs", Vec::new()),
            ],
        )
        .unwrap();
        std::fs::remove_dir(&skill).unwrap();

        let mut app = Dashboard::new(ProjectManager::new(&data).unwrap()).unwrap();
        app.handle_key(KeyCode::Char('p'));
        let backend = render(&app);
        assert_rendered_color(&backend, "✕ problem", ERROR);
        rendered_text_position(&backend, "1 with problems");
        rendered_text_position(&backend, "missing");

        // Remove the missing skill and save: the profile becomes valid again.
        app.handle_key(KeyCode::Enter);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        }
        app.handle_key(KeyCode::Delete);
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::Profiles, "{:?}", app.error);
        let catalog = crate::agent_profiles::AgentProfileCatalog::load(&data).unwrap();
        assert!(catalog.profiles()[0].skills.is_empty());

        // Delete needs confirmation.
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char('d'));
        rendered_text_position(&render(&app), "Delete profile docs?");
        app.handle_key(KeyCode::Char('n'));
        assert_eq!(
            crate::agent_profiles::load_profiles_for_editing(&data)
                .unwrap()
                .len(),
            2
        );
        app.handle_key(KeyCode::Char('d'));
        app.handle_key(KeyCode::Char('y'));
        let remaining = crate::agent_profiles::load_profiles_for_editing(&data).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            vec!["broken"]
        );
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.screen, Screen::Projects);
    }
}
