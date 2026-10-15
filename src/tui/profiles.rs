//! Agent profile management: the profile list, the profile editor, and their drawing.
//! The dashboard routes keys here and performs the load, save, and delete.

use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Padding, Paragraph, Row, Table, TableState, Wrap},
};

use super::{
    ERROR, MUTED, PRIMARY, SUCCESS, WARNING, draw_empty_state, form_field, key_hint, panel,
    selected_row_style, table_header, terminal_safe, truncate_end,
};
use crate::{
    agent_profiles::{AgentProfile, McpProfile, load_profiles_for_editing},
    model::WorkflowStep,
};

/// Workflow steps that run agents, in workflow order.
const AGENT_STEPS: [WorkflowStep; 5] = [
    WorkflowStep::SpecificationGeneration,
    WorkflowStep::CodeGeneration,
    WorkflowStep::CodeReview,
    WorkflowStep::Verification,
    WorkflowStep::Documentation,
];

const MAX_FIELD_CHARS: usize = 4_096;
/// Column where nested list items start, matching `form_field`'s marker and label width.
const LIST_INDENT: &str = "                  ";

/// Profiles as stored on disk, with each one's validation problem.
#[derive(Default)]
pub(super) struct ProfileList {
    pub(super) profiles: Vec<AgentProfile>,
    problems: Vec<Option<String>>,
    pub(super) selected: usize,
    load_error: Option<String>,
}

impl ProfileList {
    /// Reloads from disk and keeps the selection on the same profile when it still exists.
    pub(super) fn reload(&mut self, data_dir: &Path) {
        let selected_id = self.selected().map(|profile| profile.id.clone());
        match load_profiles_for_editing(data_dir) {
            Ok(profiles) => {
                self.problems = profiles
                    .iter()
                    .map(|profile| profile.validate().err().map(|error| error.to_string()))
                    .collect();
                self.profiles = profiles;
                self.load_error = None;
            }
            Err(error) => {
                self.profiles.clear();
                self.problems.clear();
                self.load_error = Some(error.to_string());
            }
        }
        match selected_id {
            Some(id) => self.select_id(&id),
            None => self.selected = self.selected.min(self.profiles.len().saturating_sub(1)),
        }
    }

    pub(super) fn select_id(&mut self, id: &str) {
        self.selected = self
            .profiles
            .iter()
            .position(|profile| profile.id == id)
            .unwrap_or_else(|| self.selected.min(self.profiles.len().saturating_sub(1)));
    }

    pub(super) fn selected(&self) -> Option<&AgentProfile> {
        self.profiles.get(self.selected)
    }

    pub(super) fn move_selection(&mut self, down: bool) {
        self.selected = if down {
            self.selected
                .saturating_add(1)
                .min(self.profiles.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }

    /// A catalog that cannot even be parsed cannot be edited safely.
    pub(super) fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EditorField {
    Id,
    Description,
    Steps,
    Model,
    Timeout,
    Skills,
    Extensions,
    McpAdapter,
    McpConfig,
}

impl EditorField {
    const ALL: [Self; 9] = [
        Self::Id,
        Self::Description,
        Self::Steps,
        Self::Model,
        Self::Timeout,
        Self::Skills,
        Self::Extensions,
        Self::McpAdapter,
        Self::McpConfig,
    ];

    fn offset(self, by: isize) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0) as isize;
        let count = Self::ALL.len() as isize;
        Self::ALL[(index + by).rem_euclid(count) as usize]
    }

    fn help(self) -> &'static str {
        match self {
            Self::Id => {
                "Stable name the step planner uses to pick this profile for an agent. Lowercase letters, digits, - and _, starting with a letter."
            }
            Self::Description => {
                "Shown to the step planner when it chooses a profile. Say what kind of agent this is for."
            }
            Self::Steps => "Workflow steps whose agents may use this profile.",
            Self::Model => {
                "Pi model for these agents, passed as --model. Leave empty for Pi's default."
            }
            Self::Timeout => {
                "Minutes before an agent is cancelled, 1-480. Leave empty for the step default: 60 for code generation and verification, 30 otherwise."
            }
            Self::Skills => {
                "Skill files or directories, passed as --skill. Use absolute paths; ~/ expands to your home directory."
            }
            Self::Extensions => {
                "Pi extension files or directories, passed as --extension. Extensions run with your permissions; review them first."
            }
            Self::McpAdapter => {
                "Pi MCP adapter extension, such as an installed pi-mcp-adapter. Set it together with the MCP config."
            }
            Self::McpConfig => {
                "JSON file with a nonempty mcpServers object, used only by these agents. Keep secrets out of it; use environment variables."
            }
        }
    }
}

pub(super) enum EditorAction {
    None,
    Save,
    Cancel,
}

/// Editable copy of one profile; nothing is written until it is saved.
pub(super) struct ProfileEditor {
    /// ID of the profile being edited, or None for a new profile.
    pub(super) original_id: Option<String>,
    id: String,
    description: String,
    steps: Vec<WorkflowStep>,
    model: String,
    timeout: String,
    skills: Vec<String>,
    extensions: Vec<String>,
    mcp_adapter: String,
    mcp_config: String,
    pub(super) focus: EditorField,
    step_cursor: usize,
    list_cursor: usize,
    /// Path being typed for the focused skills or extensions list.
    input: String,
}

impl ProfileEditor {
    pub(super) fn new() -> Self {
        Self {
            original_id: None,
            id: String::new(),
            description: String::new(),
            steps: Vec::new(),
            model: String::new(),
            timeout: String::new(),
            skills: Vec::new(),
            extensions: Vec::new(),
            mcp_adapter: String::new(),
            mcp_config: String::new(),
            focus: EditorField::Id,
            step_cursor: 0,
            list_cursor: 0,
            input: String::new(),
        }
    }

    pub(super) fn edit(profile: &AgentProfile) -> Self {
        let path_text = |path: &PathBuf| path.to_string_lossy().into_owned();
        Self {
            original_id: Some(profile.id.clone()),
            id: profile.id.clone(),
            description: profile.description.clone(),
            steps: AGENT_STEPS
                .into_iter()
                .filter(|step| profile.steps.iter().any(|value| value == step.as_str()))
                .collect(),
            model: profile.model.clone().unwrap_or_default(),
            timeout: profile
                .timeout_minutes
                .map(|minutes| minutes.to_string())
                .unwrap_or_default(),
            skills: profile.skills.iter().map(path_text).collect(),
            extensions: profile.extensions.iter().map(path_text).collect(),
            mcp_adapter: profile
                .mcp
                .as_ref()
                .map(|mcp| path_text(&mcp.adapter))
                .unwrap_or_default(),
            mcp_config: profile
                .mcp
                .as_ref()
                .map(|mcp| path_text(&mcp.config))
                .unwrap_or_default(),
            ..Self::new()
        }
    }

    /// The profile as typed; full validation happens when it is saved.
    pub(super) fn to_profile(&self) -> Result<AgentProfile, String> {
        let timeout_minutes = match self.timeout.trim() {
            "" => None,
            value => Some(
                value
                    .parse::<u32>()
                    .map_err(|_| "The time limit must be a whole number of minutes.".to_owned())?,
            ),
        };
        let mcp = match (self.mcp_adapter.trim(), self.mcp_config.trim()) {
            ("", "") => None,
            ("", _) | (_, "") => {
                return Err("Set both the MCP adapter and the MCP config, or neither.".to_owned());
            }
            (adapter, config) => Some(McpProfile {
                adapter: expand_home(adapter),
                config: expand_home(config),
            }),
        };
        let model = self.model.trim();
        Ok(AgentProfile {
            id: self.id.trim().to_owned(),
            description: self.description.trim().to_owned(),
            steps: self
                .steps
                .iter()
                .map(|step| step.as_str().to_owned())
                .collect(),
            skills: self.skills.iter().map(|path| expand_home(path)).collect(),
            extensions: self
                .extensions
                .iter()
                .map(|path| expand_home(path))
                .collect(),
            mcp,
            model: (!model.is_empty()).then(|| model.to_owned()),
            timeout_minutes,
        })
    }

    fn focused_text(&mut self) -> Option<&mut String> {
        match self.focus {
            EditorField::Id => Some(&mut self.id),
            EditorField::Description => Some(&mut self.description),
            EditorField::Model => Some(&mut self.model),
            EditorField::Timeout => Some(&mut self.timeout),
            EditorField::McpAdapter => Some(&mut self.mcp_adapter),
            EditorField::McpConfig => Some(&mut self.mcp_config),
            EditorField::Skills | EditorField::Extensions => Some(&mut self.input),
            EditorField::Steps => None,
        }
    }

    fn focused_list(&mut self) -> Option<&mut Vec<String>> {
        match self.focus {
            EditorField::Skills => Some(&mut self.skills),
            EditorField::Extensions => Some(&mut self.extensions),
            _ => None,
        }
    }

    fn set_focus(&mut self, focus: EditorField) {
        self.focus = focus;
        self.list_cursor = 0;
        self.input.clear();
    }

    pub(super) fn handle_key(&mut self, key: KeyCode) -> EditorAction {
        match key {
            KeyCode::Esc => return EditorAction::Cancel,
            KeyCode::Tab => self.set_focus(self.focus.offset(1)),
            KeyCode::BackTab => self.set_focus(self.focus.offset(-1)),
            KeyCode::Enter => {
                let input = self.input.trim().to_owned();
                match self.focused_list() {
                    Some(list) if !input.is_empty() => {
                        list.push(input);
                        let last = list.len() - 1;
                        self.list_cursor = last;
                        self.input.clear();
                    }
                    _ => return EditorAction::Save,
                }
            }
            KeyCode::Delete => {
                let cursor = self.list_cursor;
                if let Some(list) = self.focused_list()
                    && cursor < list.len()
                {
                    list.remove(cursor);
                    let remaining = list.len();
                    self.list_cursor = cursor.min(remaining.saturating_sub(1));
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let down = key == KeyCode::Down;
                let step = |cursor: usize, len: usize| {
                    if down {
                        cursor.saturating_add(1).min(len.saturating_sub(1))
                    } else {
                        cursor.saturating_sub(1)
                    }
                };
                match self.focus {
                    EditorField::Steps => {
                        self.step_cursor = step(self.step_cursor, AGENT_STEPS.len());
                    }
                    EditorField::Skills | EditorField::Extensions => {
                        let len = self.focused_list().map_or(0, |list| list.len());
                        self.list_cursor = step(self.list_cursor, len);
                    }
                    _ => self.set_focus(self.focus.offset(if down { 1 } else { -1 })),
                }
            }
            KeyCode::Char(' ') if self.focus == EditorField::Steps => {
                let step = AGENT_STEPS[self.step_cursor];
                if let Some(index) = self.steps.iter().position(|selected| *selected == step) {
                    self.steps.remove(index);
                } else {
                    self.steps.push(step);
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = self.focused_text() {
                    text.pop();
                }
            }
            KeyCode::Char(character) if !character.is_control() => {
                let numeric_only = self.focus == EditorField::Timeout;
                if let Some(text) = self.focused_text()
                    && text.chars().count() < MAX_FIELD_CHARS
                    && (!numeric_only || character.is_ascii_digit())
                {
                    text.push(character);
                }
            }
            _ => {}
        }
        EditorAction::None
    }

    pub(super) fn hints(&self) -> &'static str {
        match self.focus {
            EditorField::Steps => {
                "Tab/Shift+Tab fields  ↑/↓ step  Space toggle  Enter save  Esc cancel"
            }
            EditorField::Skills | EditorField::Extensions => {
                "Tab/Shift+Tab fields  type path  Enter add/save  ↑/↓ select  Delete remove  Esc cancel"
            }
            _ => "Tab/Shift+Tab or ↑/↓ fields  Enter save  Esc cancel",
        }
    }

    pub(super) fn title(&self) -> String {
        match &self.original_id {
            Some(id) => format!("Edit profile · {}", terminal_safe(id)),
            None => "New agent profile".to_owned(),
        }
    }
}

/// Label line for a list field: like a form field, but without a text cursor, because typing
/// goes to the list's own input line.
fn list_label(focused: bool, label: &str, note: &str) -> Line<'static> {
    Line::from(vec![
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
        Span::styled(
            note.to_owned(),
            Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
        ),
    ])
}

/// Expands a leading `~/` to the home directory; other paths are kept as typed.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

fn path_exists(path: &Path) -> bool {
    path.is_absolute() && path.exists()
}

/// `✓ path` when the path exists, `✕ path (missing)` otherwise.
fn path_line(prefix: Span<'static>, path: &Path, selected: bool) -> Line<'static> {
    let exists = path_exists(path);
    let mut spans = vec![
        prefix,
        Span::styled(
            if exists { "✓ " } else { "✕ " },
            Style::default().fg(if exists { SUCCESS } else { ERROR }),
        ),
        Span::styled(
            terminal_safe(&path.to_string_lossy()),
            if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
    ];
    if !exists {
        spans.push(Span::styled(
            if path.is_absolute() {
                "  missing"
            } else {
                "  not an absolute path"
            },
            Style::default().fg(ERROR),
        ));
    }
    Line::from(spans)
}

fn step_label(step: WorkflowStep) -> &'static str {
    match step {
        WorkflowStep::SpecificationGeneration => "specs",
        WorkflowStep::CodeGeneration => "code",
        WorkflowStep::CodeReview => "review",
        WorkflowStep::Verification => "verify",
        WorkflowStep::Documentation => "docs",
        _ => "",
    }
}

pub(super) fn draw_profiles(frame: &mut Frame, list: &ProfileList, area: Rect) {
    if let Some(error) = list.load_error() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "The profile catalog cannot be read, so it cannot be edited here.",
                    Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
                ),
                Line::default(),
                Line::raw(terminal_safe(error)),
                Line::default(),
                Line::styled(
                    "Fix or move agent-profiles.json in the data directory, then press r.",
                    Style::default().fg(MUTED),
                ),
            ])
            .wrap(Wrap { trim: true })
            .block(panel("Agent profiles").padding(Padding::horizontal(1))),
            area,
        );
        return;
    }
    if list.profiles.is_empty() {
        draw_empty_state(
            frame,
            panel("Agent profiles"),
            area,
            key_hint(
                "No agent profiles yet. Agents run without extra skills, extensions, or MCP servers. Press ",
                "n",
                " to create one.",
            ),
        );
        return;
    }
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(12)])
        .split(area);
    let invalid = list
        .problems
        .iter()
        .filter(|problem| problem.is_some())
        .count();
    let summary = if invalid > 0 {
        Span::styled(
            format!(" {invalid} with problems "),
            Style::default().fg(ERROR),
        )
    } else {
        Span::styled(
            format!(" {} profiles ", list.profiles.len()),
            Style::default().fg(MUTED),
        )
    };
    let rows = list
        .profiles
        .iter()
        .zip(&list.problems)
        .map(|(profile, problem)| {
            let steps = AGENT_STEPS
                .into_iter()
                .filter(|step| profile.steps.iter().any(|value| value == step.as_str()))
                .map(step_label)
                .collect::<Vec<_>>()
                .join(" · ");
            let count = |count: usize| {
                Span::styled(
                    count.to_string(),
                    Style::default().fg(if count == 0 { MUTED } else { PRIMARY }),
                )
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    terminal_safe(&profile.id),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(steps),
                Cell::from(count(profile.skills.len())),
                Cell::from(count(profile.extensions.len())),
                Cell::from(if profile.mcp.is_some() {
                    Span::styled("✓", Style::default().fg(PRIMARY))
                } else {
                    Span::styled("—", Style::default().fg(MUTED))
                }),
                Cell::from(profile.model.as_deref().map_or_else(
                    || Span::styled("default", Style::default().fg(MUTED)),
                    |model| Span::raw(truncate_end(&terminal_safe(model), 24)),
                )),
                Cell::from(profile.timeout_minutes.map_or_else(
                    || Span::styled("default", Style::default().fg(MUTED)),
                    |minutes| Span::raw(format!("{minutes}m")),
                )),
                Cell::from(match problem {
                    None => Span::styled("✓ ready", Style::default().fg(SUCCESS)),
                    Some(_) => Span::styled(
                        "✕ problem",
                        Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
                    ),
                }),
            ])
        });
    let mut state = TableState::default().with_selected(Some(list.selected));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Fill(1),
                Constraint::Fill(2),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(3),
                Constraint::Length(24),
                Constraint::Length(7),
                Constraint::Length(9),
            ],
        )
        .header(table_header(&[
            "ID",
            "Steps",
            "Skills",
            "Extensions",
            "MCP",
            "Model",
            "Limit",
            "State",
        ]))
        .column_spacing(2)
        .block(panel("Agent profiles").title_top(Line::from(summary).right_aligned()))
        .highlight_symbol("▌")
        .row_highlight_style(selected_row_style()),
        areas[0],
        &mut state,
    );

    let Some(profile) = list.selected() else {
        return;
    };
    let label =
        |text: &'static str| Span::styled(format!("{text:<12}"), Style::default().fg(MUTED));
    let mut lines = vec![Line::from(vec![
        label("Description"),
        Span::raw(terminal_safe(&profile.description)),
    ])];
    if let Some(Some(problem)) = list.problems.get(list.selected) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "Problem"), Style::default().fg(ERROR)),
            Span::styled(terminal_safe(problem), Style::default().fg(ERROR)),
        ]));
    }
    for (title, paths) in [
        ("Skills", &profile.skills),
        ("Extensions", &profile.extensions),
    ] {
        if paths.is_empty() {
            lines.push(Line::from(vec![
                label(title),
                Span::styled("none", Style::default().fg(MUTED)),
            ]));
        }
        for (index, path) in paths.iter().enumerate() {
            lines.push(path_line(
                label(if index == 0 { title } else { "" }),
                path,
                false,
            ));
        }
    }
    match &profile.mcp {
        Some(mcp) => {
            lines.push(path_line(label("MCP adapter"), &mcp.adapter, false));
            lines.push(path_line(label("MCP config"), &mcp.config, false));
        }
        None => lines.push(Line::from(vec![
            label("MCP"),
            Span::styled("none", Style::default().fg(MUTED)),
        ])),
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            panel(format!("{} · resources", terminal_safe(&profile.id)))
                .padding(Padding::horizontal(1)),
        ),
        areas[1],
    );
}

pub(super) fn draw_editor(frame: &mut Frame, editor: &ProfileEditor, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(40)])
        .split(area);
    let mut lines = Vec::new();
    let mut focus_line = 0;
    let mut mark_focus = |lines: &Vec<Line<'static>>, field: EditorField| {
        if editor.focus == field {
            focus_line = lines.len();
        }
    };

    mark_focus(&lines, EditorField::Id);
    lines.push(form_field(
        editor.focus == EditorField::Id,
        "ID",
        &editor.id,
        "reviewer",
    ));
    mark_focus(&lines, EditorField::Description);
    lines.push(form_field(
        editor.focus == EditorField::Description,
        "Description",
        &editor.description,
        "what these agents do",
    ));
    lines.push(Line::default());

    let steps_focused = editor.focus == EditorField::Steps;
    mark_focus(&lines, EditorField::Steps);
    lines.push(list_label(
        steps_focused,
        "Steps",
        "where agents may use it",
    ));
    for (index, step) in AGENT_STEPS.into_iter().enumerate() {
        let checked = editor.steps.contains(&step);
        let cursor = steps_focused && editor.step_cursor == index;
        lines.push(Line::from(vec![
            Span::raw(&LIST_INDENT[2..]),
            Span::styled(
                if cursor { "› " } else { "  " },
                Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if checked { "■ " } else { "□ " },
                Style::default().fg(if checked { PRIMARY } else { MUTED }),
            ),
            Span::styled(
                step.as_str(),
                if cursor {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    lines.push(Line::default());

    mark_focus(&lines, EditorField::Model);
    lines.push(form_field(
        editor.focus == EditorField::Model,
        "Model",
        &editor.model,
        "Pi default",
    ));
    mark_focus(&lines, EditorField::Timeout);
    lines.push(form_field(
        editor.focus == EditorField::Timeout,
        "Time limit",
        &editor.timeout,
        "minutes; empty uses the step default",
    ));
    lines.push(Line::default());

    for (field, title, paths) in [
        (EditorField::Skills, "Skills", &editor.skills),
        (EditorField::Extensions, "Extensions", &editor.extensions),
    ] {
        let focused = editor.focus == field;
        mark_focus(&lines, field);
        lines.push(list_label(
            focused,
            title,
            if paths.is_empty() { "none" } else { "" },
        ));
        for (index, path) in paths.iter().enumerate() {
            let selected = focused && editor.list_cursor == index;
            lines.push(path_line(
                Span::styled(
                    format!(
                        "{}{}",
                        &LIST_INDENT[2..],
                        if selected { "› " } else { "  " }
                    ),
                    Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
                ),
                &expand_home(path),
                selected,
            ));
        }
        if focused {
            let mut add = vec![
                Span::styled(
                    format!("{LIST_INDENT}+ "),
                    Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
                ),
                Span::raw(terminal_safe(&editor.input)),
                Span::styled("█", Style::default().fg(PRIMARY)),
            ];
            if editor.input.is_empty() {
                add.push(Span::styled(
                    "type a path and press Enter to add it",
                    Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::from(add));
        }
    }
    lines.push(Line::default());

    mark_focus(&lines, EditorField::McpAdapter);
    lines.push(form_field(
        editor.focus == EditorField::McpAdapter,
        "MCP adapter",
        &editor.mcp_adapter,
        "none",
    ));
    mark_focus(&lines, EditorField::McpConfig);
    lines.push(form_field(
        editor.focus == EditorField::McpConfig,
        "MCP config",
        &editor.mcp_config,
        "none",
    ));
    for (value, field) in [
        (&editor.mcp_adapter, EditorField::McpAdapter),
        (&editor.mcp_config, EditorField::McpConfig),
    ] {
        if !value.trim().is_empty() && !path_exists(&expand_home(value.trim())) {
            lines.push(Line::from(vec![
                Span::raw(LIST_INDENT),
                Span::styled(
                    format!(
                        "✕ {} path is missing or not absolute",
                        if field == EditorField::McpAdapter {
                            "adapter"
                        } else {
                            "config"
                        }
                    ),
                    Style::default().fg(ERROR),
                ),
            ]));
        }
    }

    // Keep the focused field visible on short terminals.
    let visible = usize::from(columns[0].height.saturating_sub(2));
    let scroll = focus_line.saturating_sub(visible.saturating_sub(8));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(0), 0))
            .block(panel(editor.title()).padding(Padding::new(1, 1, 1, 0))),
        columns[0],
    );

    let help = vec![
        Line::styled(
            match editor.focus {
                EditorField::Id => "ID",
                EditorField::Description => "Description",
                EditorField::Steps => "Steps",
                EditorField::Model => "Model",
                EditorField::Timeout => "Time limit",
                EditorField::Skills => "Skills",
                EditorField::Extensions => "Extensions",
                EditorField::McpAdapter => "MCP adapter",
                EditorField::McpConfig => "MCP config",
            },
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        Line::raw(editor.focus.help()),
        Line::default(),
        Line::styled(
            "Saving checks every path. Agents already running keep their resources; the next step uses the saved profile.",
            Style::default().fg(MUTED),
        ),
        Line::default(),
        Line::styled(
            "Cronos-AI never installs skills, extensions, or MCP servers.",
            Style::default().fg(WARNING),
        ),
    ];
    frame.render_widget(
        Paragraph::new(help)
            .wrap(Wrap { trim: true })
            .block(panel("Help").padding(Padding::horizontal(1))),
        columns[1],
    );
}
