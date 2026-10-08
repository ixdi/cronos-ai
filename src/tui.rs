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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    model::{AgentActivity, AgentStatus, Priority, Project, ProjectStatus, Task, TaskStatus},
    project::{ProjectError, ProjectManager},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Projects,
    Tasks,
    TaskDetail,
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
        if key == KeyCode::Char('q') {
            self.should_quit = true;
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
}

pub fn run(data_dir: PathBuf) -> Result<(), Box<dyn Error>> {
    let manager = ProjectManager::new(data_dir)?;
    let mut app = Dashboard::new(manager)?;
    let _terminal_guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut last_refresh = Instant::now();

    while !app.should_quit {
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
        Screen::Tasks => "↑/↓ select  Enter details  Backspace projects  r refresh  q quit",
        Screen::TaskDetail => "↑/↓ activity  Backspace tasks  r refresh  q quit",
    };
    let footer = app.error.as_deref().map_or_else(
        || help.to_owned(),
        |error| format!("{help}  |  Error: {}", terminal_safe(error)),
    );
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::TOP)),
        areas[1],
    );
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
        .constraints([Constraint::Length(7), Constraint::Min(3)])
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
    ]);
    frame.render_widget(
        Paragraph::new(summary)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(title)),
        areas[0],
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
        areas[1],
        &mut state,
    );
}

fn agent_activity_item(activity: &AgentActivity) -> ListItem<'static> {
    let detail = terminal_safe(activity.detail.as_deref().unwrap_or(""));
    ListItem::new(format!(
        "{}  {} — {} / {}  {}",
        activity.updated_at_unix_secs,
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
    use super::{Dashboard, Screen};
    use crate::{
        model::{AgentStatus, Priority, TaskStatus},
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
        writer
            .set_task_status(project.id, task.id, TaskStatus::InProgress)
            .unwrap();
        writer
            .set_task_step(project.id, task.id, "spec-generation".to_owned())
            .unwrap();
        writer
            .record_agent_activity(
                project.id,
                task.id,
                "spec-writer".to_owned(),
                "spec-generation".to_owned(),
                AgentStatus::Running,
                Some("drafting proposal".to_owned()),
            )
            .unwrap();
        app.refresh().unwrap();
        let task = app.selected_task().unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.current_step.as_deref(), Some("spec-generation"));
        assert_eq!(task.agent_activity.len(), 1);
        assert_eq!(app.selected_activity, 0);

        writer
            .record_agent_activity(
                project.id,
                task.id,
                "spec-writer".to_owned(),
                "spec-generation".to_owned(),
                AgentStatus::Completed,
                Some("draft ready".to_owned()),
            )
            .unwrap();
        app.refresh().unwrap();
        assert_eq!(app.selected_activity, 1);
    }
}
