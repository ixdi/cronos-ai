//! Kanban board of a project's tasks by status, and the project's progress analytics.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use super::{
    MUTED, PRIMARY, SUCCESS, elapsed_label, panel, priority_badge, selected_row_style,
    step_elapsed, task_status_color, task_status_label, terminal_safe, truncate_end,
};
use crate::model::{Task, TaskStatus};

/// Board columns, in the order work moves through them.
pub(super) const BOARD_COLUMNS: [TaskStatus; 5] = [
    TaskStatus::Pending,
    TaskStatus::InProgress,
    TaskStatus::Error,
    TaskStatus::Completed,
    TaskStatus::Rejected,
];

/// How the task list is shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TaskView {
    #[default]
    Table,
    Board,
}

/// Indexes into `tasks` for each board column, keeping the list order inside a column.
pub(super) fn board_columns(tasks: &[Task]) -> [Vec<usize>; 5] {
    let mut columns: [Vec<usize>; 5] = Default::default();
    for (index, task) in tasks.iter().enumerate() {
        if let Some(column) = BOARD_COLUMNS
            .iter()
            .position(|status| *status == task.status)
        {
            columns[column].push(index);
        }
    }
    columns
}

/// The task selected after an arrow key on the board. Left and right skip empty columns and keep
/// the row where possible; other keys leave the selection unchanged.
pub(super) fn move_on_board(tasks: &[Task], selected: usize, key: KeyCode) -> usize {
    let columns = board_columns(tasks);
    let Some((column, row)) = columns.iter().enumerate().find_map(|(column, cards)| {
        cards
            .iter()
            .position(|index| *index == selected)
            .map(|row| (column, row))
    }) else {
        return selected;
    };
    match key {
        KeyCode::Up => columns[column][row.saturating_sub(1)],
        KeyCode::Down => columns[column][(row + 1).min(columns[column].len() - 1)],
        KeyCode::Left | KeyCode::Right => {
            let mut candidates: Box<dyn Iterator<Item = usize>> = if key == KeyCode::Right {
                Box::new(column + 1..columns.len())
            } else {
                Box::new((0..column).rev())
            };
            candidates
                .find(|candidate| !columns[*candidate].is_empty())
                .map_or(selected, |target| {
                    let cards = &columns[target];
                    cards[row.min(cards.len() - 1)]
                })
        }
        _ => selected,
    }
}

/// Progress figures for one project's tasks.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct ProjectAnalytics {
    pub total: usize,
    pub completed: usize,
    pub rejected: usize,
    pub errors: usize,
    /// Mean time from a completed task's first recorded step to its completion.
    pub average_completion_secs: Option<u64>,
}

impl ProjectAnalytics {
    pub fn from_tasks(tasks: &[Task]) -> Self {
        let count = |status: TaskStatus| tasks.iter().filter(|task| task.status == status).count();
        let durations = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .filter_map(|task| {
                let started = task.step_started_at_unix_secs.values().min()?;
                Some(task.updated_at_unix_secs.saturating_sub(*started))
            })
            .collect::<Vec<_>>();
        Self {
            total: tasks.len(),
            completed: count(TaskStatus::Completed),
            rejected: count(TaskStatus::Rejected),
            errors: count(TaskStatus::Error),
            average_completion_secs: (!durations.is_empty())
                .then(|| durations.iter().sum::<u64>() / durations.len() as u64),
        }
    }

    /// Tasks that still count toward the project: everything except abandoned work.
    pub fn active_total(&self) -> usize {
        self.total - self.rejected
    }

    pub fn percent_done(&self) -> usize {
        match self.active_total() {
            0 => 0,
            total => self.completed * 100 / total,
        }
    }
}

/// One-line project summary: a progress bar, completion, average completion time, and errors.
pub(super) fn analytics_line(analytics: &ProjectAnalytics) -> Line<'static> {
    const BAR: usize = 10;
    let filled = analytics.percent_done() * BAR / 100;
    let mut spans = vec![
        Span::styled("█".repeat(filled), Style::default().fg(SUCCESS)),
        Span::styled("░".repeat(BAR - filled), Style::default().fg(MUTED)),
        Span::raw(format!(
            " {}/{} done ({}%)",
            analytics.completed,
            analytics.active_total(),
            analytics.percent_done()
        )),
    ];
    if let Some(average) = analytics.average_completion_secs {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        spans.push(Span::raw(format!(
            "avg {} to complete",
            elapsed_label(average)
        )));
    }
    if analytics.errors > 0 {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        spans.push(Span::styled(
            format!(
                "{} {}",
                analytics.errors,
                if analytics.errors == 1 {
                    "error"
                } else {
                    "errors"
                }
            ),
            Style::default().fg(task_status_color(&TaskStatus::Error)),
        ));
    }
    if analytics.rejected > 0 {
        spans.push(Span::styled(
            format!(" · {} abandoned", analytics.rejected),
            Style::default().fg(MUTED),
        ));
    }
    // Pad like a panel title so the summary never touches the corner.
    spans.insert(0, Span::raw(" "));
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Draws the board: one column per status, a two-line card per task, the selection marked.
pub(super) fn draw_board(
    frame: &mut Frame,
    tasks: &[Task],
    selected: usize,
    block: Block,
    area: Rect,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Abandoned work is rare; its column appears only when there is something in it.
    let shown = BOARD_COLUMNS
        .iter()
        .zip(board_columns(tasks))
        .filter(|(status, cards)| **status != TaskStatus::Rejected || !cards.is_empty())
        .collect::<Vec<_>>();
    let areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(shown.iter().map(|_| Constraint::Fill(1)))
        .spacing(1)
        .split(inner);
    for ((status, cards), area) in shown.iter().zip(areas.iter()) {
        let column = panel(format!("{} {}", task_status_label(status), cards.len()))
            .border_style(Style::default().fg(task_status_color(status)));
        let card_area = column.inner(*area);
        frame.render_widget(column, *area);
        let width = usize::from(card_area.width).saturating_sub(1);
        // Each card is two lines plus a blank one; scroll so the selected card stays visible.
        let visible = (usize::from(card_area.height) + 1) / 3;
        let selected_row = cards.iter().position(|index| *index == selected);
        let first = selected_row.map_or(0, |row| (row + 1).saturating_sub(visible.max(1)));
        let mut lines = Vec::new();
        for index in cards.iter().skip(first).take(visible) {
            let task = &tasks[*index];
            let is_selected = *index == selected;
            let marker = if is_selected { "▌" } else { " " };
            let title_style = if is_selected {
                selected_row_style()
            } else {
                Style::default()
            };
            let priority = priority_badge(&task.priority);
            let symbol = priority.content.chars().next().unwrap_or(' ').to_string();
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(PRIMARY)),
                Span::styled(format!("{symbol} "), priority.style),
                Span::styled(
                    truncate_end(&terminal_safe(&task.title), width.saturating_sub(2)),
                    title_style.add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(PRIMARY)),
                Span::styled(
                    truncate_end(&card_detail(task), width),
                    Style::default().fg(MUTED),
                ),
            ]));
            lines.push(Line::default());
        }
        if cards.is_empty() {
            lines.push(Line::styled(" —", Style::default().fg(MUTED)));
        }
        frame.render_widget(Paragraph::new(lines), card_area);
    }
}

/// Second card line: the current step and its running time, or what finished the task.
fn card_detail(task: &Task) -> String {
    match task.status {
        TaskStatus::Completed => {
            let outcome = task
                .published
                .as_deref()
                .map_or_else(|| "merged".to_owned(), terminal_safe);
            match &task.release {
                Some(release) => format!("{} · {outcome}", terminal_safe(release)),
                None => outcome,
            }
        }
        TaskStatus::Rejected => "abandoned".to_owned(),
        _ => {
            let step = terminal_safe(task.current_step.as_deref().unwrap_or("not started"));
            match (task.status == TaskStatus::InProgress)
                .then(|| step_elapsed(task))
                .flatten()
            {
                Some(elapsed) => format!("{step} · {}", elapsed_label(elapsed)),
                None => step,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crossterm::event::KeyCode;

    use super::{BOARD_COLUMNS, ProjectAnalytics, board_columns, move_on_board};
    use crate::model::{Priority, ProjectId, Task, TaskId, TaskStatus};

    fn task(status: TaskStatus, started: Option<u64>, updated: u64) -> Task {
        Task {
            id: TaskId::new(),
            project_id: ProjectId::new(),
            title: "Task".to_owned(),
            description: None,
            priority: Priority::Medium,
            dependencies: Vec::new(),
            status,
            current_step: None,
            step_started_at_unix_secs: started
                .map(|at| BTreeMap::from([("specification-generation".to_owned(), at)]))
                .unwrap_or_default(),
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
            updated_at_unix_secs: updated,
        }
    }

    #[test]
    fn columns_follow_the_workflow_and_keep_task_order() {
        let tasks = vec![
            task(TaskStatus::Completed, Some(0), 10),
            task(TaskStatus::Pending, None, 0),
            task(TaskStatus::InProgress, Some(5), 5),
            task(TaskStatus::Pending, None, 0),
            task(TaskStatus::Error, Some(1), 2),
        ];

        let columns = board_columns(&tasks);

        assert_eq!(
            BOARD_COLUMNS,
            [
                TaskStatus::Pending,
                TaskStatus::InProgress,
                TaskStatus::Error,
                TaskStatus::Completed,
                TaskStatus::Rejected,
            ]
        );
        assert_eq!(columns, [vec![1, 3], vec![2], vec![4], vec![0], vec![]]);
    }

    #[test]
    fn arrows_move_within_a_column_and_skip_empty_columns() {
        let tasks = vec![
            task(TaskStatus::Pending, None, 0),
            task(TaskStatus::Pending, None, 0),
            task(TaskStatus::Pending, None, 0),
            task(TaskStatus::Completed, Some(0), 1),
        ];

        assert_eq!(move_on_board(&tasks, 0, KeyCode::Down), 1);
        assert_eq!(move_on_board(&tasks, 2, KeyCode::Down), 2);
        assert_eq!(move_on_board(&tasks, 0, KeyCode::Up), 0);
        // In Progress and Error are empty, so Right jumps straight to Completed.
        assert_eq!(move_on_board(&tasks, 2, KeyCode::Right), 3);
        // Moving back keeps the row when the column is long enough, else the last card.
        assert_eq!(move_on_board(&tasks, 3, KeyCode::Left), 0);
        assert_eq!(move_on_board(&tasks, 3, KeyCode::Right), 3);
    }

    #[test]
    fn analytics_count_progress_and_average_completion_time() {
        let tasks = vec![
            task(TaskStatus::Completed, Some(100), 100 + 3_600),
            task(TaskStatus::Completed, Some(0), 1_800),
            task(TaskStatus::InProgress, Some(50), 60),
            task(TaskStatus::Error, Some(50), 60),
            task(TaskStatus::Rejected, None, 0),
        ];

        let analytics = ProjectAnalytics::from_tasks(&tasks);

        assert_eq!(analytics.total, 5);
        assert_eq!(analytics.completed, 2);
        assert_eq!(analytics.errors, 1);
        // Rejected work is abandoned, so it does not count against progress.
        assert_eq!(analytics.active_total(), 4);
        assert_eq!(analytics.percent_done(), 50);
        assert_eq!(analytics.average_completion_secs, Some(2_700));
    }

    #[test]
    fn analytics_without_completed_tasks_have_no_average() {
        let analytics = ProjectAnalytics::from_tasks(&[task(TaskStatus::Pending, None, 0)]);

        assert_eq!(analytics.percent_done(), 0);
        assert_eq!(analytics.average_completion_secs, None);
        assert_eq!(ProjectAnalytics::from_tasks(&[]).percent_done(), 0);
    }
}
