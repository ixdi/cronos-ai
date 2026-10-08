use std::{
    error::Error,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    integrations::{PiRpcHandle, ToolCommands, herdr_session_available},
    model::{
        AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, Priority, Project,
        ProjectStatus, Task, TaskStatus, WorkflowStep,
    },
    project::{ProjectError, ProjectManager},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Projects,
    Tasks,
    TaskDetail,
}

enum Dialog {
    None,
    Revision { gate: ApprovalGate, note: String },
    Clarify { note: String },
    Abandon { note: String },
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
        })
    }

    fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.selected_project)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected_task)
    }

    fn refresh(&mut self) -> Result<(), ProjectError> {
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
            if self.screen == Screen::TaskDetail && self.tasks.is_empty() {
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
        self.error = None;
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
            }
            return;
        }

        if self.screen == Screen::TaskDetail {
            match key {
                KeyCode::Char('y') => self.approve_active_gate(),
                KeyCode::Char('n') => self.request_revision(),
                KeyCode::Char('c') => self.open_clarify_dialog(),
                KeyCode::Char('a') => self.open_abandon_dialog(),
                _ => self.handle_navigation_key(key),
            }
            return;
        }
        self.handle_navigation_key(key);
    }

    fn handle_navigation_key(&mut self, key: KeyCode) {
        match (self.screen, key) {
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
        match self.manager.retry_task(project_id, task_id) {
            Ok(task) => {
                self.workflow_status = Some(format!(
                    "Retry scheduled for attempt {} at specification generation",
                    task.attempt
                ));
                self.refresh_for_ui();
            }
            Err(error) => self.error = Some(error.to_string()),
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

    fn handle_dialog_key(&mut self, key: KeyCode) {
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
                Dialog::None => {}
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
                if let Some((project_id, task_id)) = self.selected_ids() {
                    match self.manager.clarify_task(project_id, task_id, note) {
                        Ok(task) => {
                            self.workflow_status = Some(format!(
                                "Clarification saved for attempt {} at specification generation",
                                task.attempt
                            ));
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
        let candidate = match self.manager.ready_tasks(project_id) {
            Ok(tasks) => tasks.into_iter().next(),
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        let Some(candidate) = candidate else {
            self.error = Some("There is no ready task in this project.".to_owned());
            return;
        };
        if !herdr_session_available() {
            self.error =
                Some("Start Cronos-AI inside a Herdr-managed pane to run tasks.".to_owned());
            return;
        }
        if let Err(error) = self
            .manager
            .validated_task_worktree_path(project_id, candidate.id)
        {
            self.error = Some(error.to_string());
            return;
        }
        match self.manager.start_next_task(project_id) {
            Ok(Some(task)) => {
                if let Err(error) = self.launch_pi_for_task(project_id, task.id) {
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
        let extension = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("extensions")
            .join("cronos-workflow.ts");
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
                        && !task
                            .current_step
                            .as_deref()
                            .and_then(WorkflowStep::parse)
                            .is_some_and(WorkflowStep::requires_human_approval)
                    {
                        let _ =
                            self.manager
                                .set_task_status(project_id, task_id, TaskStatus::Error);
                        self.error = Some(
                            "Pi settled before completing the current workflow step.".to_owned(),
                        );
                        self.refresh_for_ui();
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
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(frame.area());
    match app.screen {
        Screen::Projects => draw_projects(frame, app, areas[0]),
        Screen::Tasks => draw_tasks(frame, app, areas[0]),
        Screen::TaskDetail => draw_task_detail(frame, app, areas[0]),
    }
    let help = match app.screen {
        Screen::Projects => "↑/↓ select  Enter open  r refresh  q quit",
        Screen::Tasks => {
            "↑/↓ select  Enter details  s start next ready task  Backspace projects  r refresh  q quit"
        }
        Screen::TaskDetail
            if app
                .selected_task()
                .is_some_and(|task| task.status == TaskStatus::Error) =>
        {
            "r retry from specs  c clarify context  a abandon  Backspace tasks  q quit"
        }
        Screen::TaskDetail if app.active_approval_gate().is_some() => {
            "y approve  n request revision  a abandon  ↑/↓ activity  Backspace tasks  q quit"
        }
        Screen::TaskDetail => "a abandon  ↑/↓ activity  Backspace tasks  r refresh  q quit",
    };
    let footer = match (&app.workflow_status, &app.error) {
        (Some(status), Some(error)) => format!(
            "{help}  |  {}  |  Error: {}",
            terminal_safe(status),
            terminal_safe(error)
        ),
        (Some(status), None) => format!("{help}  |  {}", terminal_safe(status)),
        (None, Some(error)) => format!("{help}  |  Error: {}", terminal_safe(error)),
        (None, None) => help.to_owned(),
    };
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::TOP)),
        areas[1],
    );
    draw_dialog(frame, app);
}

fn draw_dialog(frame: &mut Frame, app: &Dashboard) {
    let (title, prompt, note) = match &app.dialog {
        Dialog::None => return,
        Dialog::Revision { gate, note } => (
            format!("Request changes — {}", approval_gate_label(*gate)),
            "Describe the revision needed:",
            note,
        ),
        Dialog::Clarify { note } => (
            "Clarify task".to_owned(),
            "Add context for replacement specifications:",
            note,
        ),
        Dialog::Abandon { note } => (
            "Abandon task".to_owned(),
            "This marks the task Rejected. Enter the reason:",
            note,
        ),
    };
    let area = centered_rect(70, 35, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!(
            "{prompt}\n\n> {}_\n\nEnter save  Esc cancel",
            terminal_safe(note)
        ))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Percentage(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Percentage(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(vertical[1])[1]
}

fn draw_projects(frame: &mut Frame, app: &Dashboard, area: ratatui::layout::Rect) {
    let items = app
        .projects
        .iter()
        .map(|project| {
            let status = match project.status {
                ProjectStatus::Active => "active",
                ProjectStatus::Archived => "archived",
            };
            ListItem::new(format!("{}  [{status}]", terminal_safe(&project.name)))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.selected_project));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Projects"))
            .highlight_symbol("▶ ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

fn draw_tasks(frame: &mut Frame, app: &Dashboard, area: ratatui::layout::Rect) {
    let project_name = app.selected_project().map_or_else(
        || "Tasks".to_owned(),
        |project| terminal_safe(&project.name),
    );
    let items = app
        .tasks
        .iter()
        .map(|task| {
            let step = task.current_step.as_deref().unwrap_or("—");
            ListItem::new(format!(
                "{:<11} {:<6} {}  |  {}",
                task_status_label(&task.status),
                priority_label(&task.priority),
                terminal_safe(&task.title),
                terminal_safe(step),
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.selected_task));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{project_name} — tasks (status | priority | step)")),
            )
            .highlight_symbol("▶ ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

fn draw_task_detail(frame: &mut Frame, app: &Dashboard, area: ratatui::layout::Rect) {
    let Some(task) = app.selected_task() else {
        frame.render_widget(
            Paragraph::new("No task selected").block(Block::bordered()),
            area,
        );
        return;
    };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Min(2),
        ])
        .split(area);
    let description = terminal_safe(task.description.as_deref().unwrap_or("No description."));
    let current_step = terminal_safe(task.current_step.as_deref().unwrap_or("Not started"));
    let title = terminal_safe(&task.title);
    let summary = Text::from(vec![
        Line::from(vec![
            Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(task_status_label(&task.status)),
        ]),
        Line::from(vec![
            Span::styled("Priority: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(priority_label(&task.priority)),
        ]),
        Line::from(vec![
            Span::styled(
                "Active step: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(current_step),
        ]),
        Line::from(vec![
            Span::styled(
                "Description: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(description),
        ]),
        Line::from(format!("Attempt: {}", task.attempt)),
        Line::from(format!(
            "Specifications: {}  |  Final code: {}",
            approval_status(task, ApprovalGate::Specifications),
            approval_status(task, ApprovalGate::FinalCode),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(summary)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(title)),
        areas[0],
    );

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
        decisions.insert(0, Line::from(format!("Abandoned: {}", terminal_safe(note))));
    }
    if decisions.is_empty() {
        decisions.push(Line::from("No human decisions recorded."));
    }
    frame.render_widget(
        Paragraph::new(Text::from(decisions))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Human decisions and revision notes"),
            ),
        areas[1],
    );

    let items = if task.agent_activity.is_empty() {
        vec![ListItem::new("No agent activity recorded yet.")]
    } else {
        task.agent_activity
            .iter()
            .map(agent_activity_item)
            .collect::<Vec<_>>()
    };
    let mut state = ListState::default();
    if !task.agent_activity.is_empty() {
        state.select(Some(app.selected_activity));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Agent activity (newest updates appear below)"),
            )
            .highlight_symbol("▶ ")
            .highlight_style(Style::default().fg(Color::Yellow)),
        areas[2],
        &mut state,
    );
}

fn recovery_record_line(record: &crate::model::RecoveryRecord) -> Line<'static> {
    let action = match record.action {
        crate::model::RecoveryAction::Retry => "retry",
        crate::model::RecoveryAction::Clarify => "clarification",
    };
    Line::from(format!(
        "Attempt {} — {action}: {}",
        record.attempt,
        terminal_safe(record.note.as_deref().unwrap_or("")),
    ))
}

fn approval_record_line(record: &crate::model::ApprovalRecord) -> Line<'static> {
    let note = record.note.as_deref().unwrap_or("");
    Line::from(format!(
        "Attempt {} — {}: {}  {}",
        record.attempt,
        approval_gate_label(record.gate),
        approval_decision_label(record.decision),
        terminal_safe(note),
    ))
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

fn approval_status(task: &Task, gate: ApprovalGate) -> &'static str {
    task.approval_history
        .iter()
        .rev()
        .find(|record| record.attempt == task.attempt && record.gate == gate)
        .map_or("pending", |record| approval_decision_label(record.decision))
}

fn agent_activity_item(activity: &AgentActivity) -> ListItem<'static> {
    let detail = terminal_safe(activity.detail.as_deref().unwrap_or(""));
    ListItem::new(format!(
        "{}  attempt {}  {} — {} / {}  {}",
        activity.updated_at_unix_secs,
        activity.attempt,
        terminal_safe(&activity.agent_name),
        terminal_safe(&activity.step),
        agent_status_label(&activity.status),
        detail,
    ))
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
        AgentStatus::Failed => "failed",
        AgentStatus::Cancelled => "cancelled",
        AgentStatus::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests {
    use super::{Dashboard, Dialog, Screen};
    use crate::{
        model::{AgentStatus, ApprovalDecision, ApprovalGate, Priority, TaskStatus, WorkflowStep},
        project::ProjectManager,
    };
    use crossterm::event::KeyCode;

    #[test]
    fn rendered_user_content_cannot_inject_terminal_control_sequences() {
        let rendered = super::terminal_safe("task\u{1b}[2J\nname");
        assert_eq!(rendered, "task [2J name");
        assert!(!rendered.chars().any(char::is_control));
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
    fn starting_the_queue_does_not_claim_a_task_without_a_valid_worktree() {
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
}
