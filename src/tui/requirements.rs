//! Project requirements, the background planner that turns them into tasks, and the review of
//! the plan it proposes. The dashboard routes keys here and performs the saves.

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Padding, Paragraph, Row, Table, TableState, Wrap},
};

use super::{
    MUTED, PRIMARY, SUCCESS, draw_empty_state, elapsed_label, key_hint, panel, priority_badge,
    selected_row_style, table_header, terminal_safe, truncate_end,
};
use crate::{
    integrations::ToolCommands,
    model::{Project, ProjectId, RequirementKind, Task},
    planning::Plan,
    project::ProjectManager,
};

/// How long the planner may run from the TUI.
const PLANNER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(super) fn kind_label(kind: RequirementKind) -> &'static str {
    match kind {
        RequirementKind::Functional => "functional",
        RequirementKind::NonFunctional => "non-functional",
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum RequirementField {
    Text,
    Kind,
}

/// The new-requirement dialog.
pub(super) struct RequirementForm {
    pub text: String,
    pub kind: RequirementKind,
    pub focus: RequirementField,
}

impl Default for RequirementForm {
    fn default() -> Self {
        Self {
            text: String::new(),
            kind: RequirementKind::Functional,
            focus: RequirementField::Text,
        }
    }
}

/// A planner running on its own thread so the dashboard stays responsive.
pub(super) struct PlanJob {
    pub project_id: ProjectId,
    pub started: Instant,
    receiver: Receiver<Result<Plan, String>>,
}

impl PlanJob {
    /// Starts the planner with its own project manager on the same data directory.
    pub fn start(data_dir: PathBuf, project_id: ProjectId, tools: ToolCommands) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = ProjectManager::new(data_dir)
                .map_err(|error| error.to_string())
                .and_then(|manager| {
                    manager
                        .plan_tasks(project_id, &tools, PLANNER_TIMEOUT)
                        .map_err(|error| error.to_string())
                });
            let _ = sender.send(result);
        });
        Self {
            project_id,
            started: Instant::now(),
            receiver,
        }
    }

    /// The planner's result once it has finished.
    pub fn finished(&self) -> Option<Result<Plan, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("the planner stopped without an answer".to_owned()))
            }
        }
    }
}

/// The requirements table, and below it the planner's progress or the plan waiting for review.
pub(super) fn draw_requirements(
    frame: &mut Frame,
    project: &Project,
    tasks: &[Task],
    plan: Option<&Plan>,
    planning: Option<&PlanJob>,
    selected: usize,
    area: Rect,
) {
    let lower = match (planning, plan) {
        (Some(_), _) => 3,
        // Borders and the header row around one row per planned task.
        (None, Some(plan)) => (plan.tasks.len() as u16 + 3).min(area.height / 2),
        (None, None) => 0,
    };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(lower)])
        .split(area);
    draw_requirement_table(frame, project, tasks, selected, areas[0]);
    if let Some(job) = planning {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("⟳ Planning ", Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD)),
                Span::raw(elapsed_label(job.started.elapsed().as_secs())),
                Span::styled(
                    " · the planner reads the repository; nothing changes until you approve its plan",
                    Style::default().fg(MUTED),
                ),
            ]))
            .wrap(Wrap { trim: true })
            .block(panel("Planner").padding(Padding::horizontal(1))),
            areas[1],
        );
    } else if let Some(plan) = plan {
        draw_plan(frame, plan, project, areas[1]);
    }
}

fn draw_requirement_table(
    frame: &mut Frame,
    project: &Project,
    tasks: &[Task],
    selected: usize,
    area: Rect,
) {
    let block = panel(format!("{} · requirements", terminal_safe(&project.name)));
    if project.requirements.is_empty() {
        draw_empty_state(
            frame,
            block,
            area,
            key_hint("No requirements yet. Press ", "n", " to add one."),
        );
        return;
    }
    // Borders, right padding, the selection marker, column gaps, and the fixed columns.
    let text_width = usize::from(area.width).saturating_sub(33).max(10);
    let rows = project
        .requirements
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let linked = tasks
                .iter()
                .filter(|task| task.requirement_ids.contains(&requirement.id))
                .count();
            let text = wrap_words(&terminal_safe(&requirement.text), text_width);
            let height = text.len() as u16;
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("R{}", index + 1),
                    Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    kind_label(requirement.kind),
                    Style::default().fg(MUTED),
                )),
                Cell::from(text.into_iter().map(Line::from).collect::<Vec<_>>()),
                Cell::from(Span::styled(
                    match linked {
                        0 => "—".to_owned(),
                        count => count.to_string(),
                    },
                    Style::default().fg(if linked > 0 { SUCCESS } else { MUTED }),
                )),
            ])
            .height(height)
        });
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Length(14),
                Constraint::Fill(1),
                Constraint::Length(5),
            ],
        )
        .header(table_header(&["#", "Kind", "Requirement", "Tasks"]))
        .column_spacing(2)
        .block(block.padding(Padding::right(1)))
        .highlight_symbol("▌")
        .row_highlight_style(selected_row_style()),
        area,
        &mut state,
    );
}

fn draw_plan(frame: &mut Frame, plan: &Plan, project: &Project, area: Rect) {
    let references = |ids: &[crate::model::RequirementId]| {
        let mut numbers = ids
            .iter()
            .filter_map(|id| {
                project
                    .requirements
                    .iter()
                    .position(|requirement| requirement.id == *id)
                    .map(|index| index + 1)
            })
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        numbers
            .iter()
            .map(|number| format!("R{number}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // Borders, padding, column gaps, and the fixed columns; the rest is split between the
    // task title and the notes.
    let flexible = usize::from(area.width).saturating_sub(28);
    let title_width = (flexible / 2).max(8);
    let notes_width = flexible.saturating_sub(title_width).max(8);
    let rows = plan.tasks.iter().map(|task| {
        let after = task
            .depends_on
            .iter()
            .map(|index| terminal_safe(&plan.tasks[*index].title))
            .collect::<Vec<_>>();
        let mut notes = Vec::new();
        if task.existing_task.is_some() {
            notes.push("↺ existing task".to_owned());
        }
        if !after.is_empty() {
            notes.push(format!("after {}", after.join(", ")));
        }
        Row::new(vec![
            Cell::from(priority_badge(&task.priority)),
            Cell::from(truncate_end(&terminal_safe(&task.title), title_width)),
            Cell::from(Span::styled(
                references(&task.requirement_ids),
                Style::default().fg(PRIMARY),
            )),
            Cell::from(Span::styled(
                truncate_end(&notes.join("  "), notes_width),
                Style::default().fg(MUTED),
            )),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Fill(2),
                Constraint::Length(10),
                Constraint::Fill(2),
            ],
        )
        .header(table_header(&["Priority", "Task", "Covers", "Notes"]))
        .column_spacing(2)
        .block(
            panel(format!("Proposed plan · {} tasks", plan.tasks.len()))
                .border_style(Style::default().fg(PRIMARY))
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

/// Greedy word wrap to `width` columns; a word longer than the width is split.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let mut word = word.to_owned();
        loop {
            let line = lines.last_mut().expect("there is always a current line");
            let used = line.chars().count();
            let gap = usize::from(used > 0);
            if used + gap + word.chars().count() <= width {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if used > 0 {
                lines.push(String::new());
                continue;
            }
            let head = word.chars().take(width).collect::<String>();
            word = word.chars().skip(width).collect();
            line.push_str(&head);
            lines.push(String::new());
            if word.is_empty() {
                break;
            }
        }
    }
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap_words;

    #[test]
    fn words_wrap_at_the_width_and_long_words_split() {
        assert_eq!(
            wrap_words("Users can export any report", 12),
            ["Users can", "export any", "report"]
        );
        assert_eq!(wrap_words("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_words("", 4), [""]);
    }
}
