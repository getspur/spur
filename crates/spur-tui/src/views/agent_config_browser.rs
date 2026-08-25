use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::{config::GraphConfig, AgentConfig, ProfileConfig, SpurEvent};

use crate::action::Action;
use crate::configure_section::{parse_configure_arg, ConfigureSection};

use super::settings_graph::GraphPane;
use super::settings_skills::SkillsPane;
use super::settings_tui::TuiPane;
use super::{View, ViewContext};

/// Solver mins (`sol_151a6b1d3e5b409e`): sections 12, agents 16, fields 42, gutter 1.
const MIN_SECTIONS_WIDTH: u16 = 12;
const MIN_AGENTS_WIDTH: u16 = 16;
const MIN_FIELDS_WIDTH: u16 = 42;
const GUTTER: u16 = 1;
const THREE_PANE_MIN_WIDTH: u16 =
    MIN_SECTIONS_WIDTH + GUTTER + MIN_AGENTS_WIDTH + GUTTER + MIN_FIELDS_WIDTH;
const TWO_PANE_MIN_WIDTH: u16 = MIN_SECTIONS_WIDTH + GUTTER + MIN_AGENTS_WIDTH;
const SECTIONS_STACK_HEIGHT: u16 = 6;
const STATUS_HEIGHT: u16 = 1;
const STATUS_MAX_COLS: usize = 40;
const STATUS_IDLE: &str = "Tab  j/k  Enter  s  Esc";
const STATUS_EDITING: &str = "Enter save  Esc cancel";
const ERROR_FG: Color = Color::LightRed;

const FIELD_ORDER: [EditableAgentField; 8] = [
    EditableAgentField::AdditionalDirectories,
    EditableAgentField::Args,
    EditableAgentField::Capabilities,
    EditableAgentField::SkipPermissions,
    EditableAgentField::SkipPermissionsArgs,
    EditableAgentField::SkipPermissionsSessionMode,
    EditableAgentField::ProfileSelect,
    EditableAgentField::ProfileMaterialize,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserPane {
    Sections,
    Agents,
    Fields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableAgentField {
    AdditionalDirectories,
    Args,
    Capabilities,
    SkipPermissions,
    SkipPermissionsArgs,
    SkipPermissionsSessionMode,
    ProfileSelect,
    ProfileMaterialize,
}

impl EditableAgentField {
    fn label(self) -> &'static str {
        match self {
            Self::AdditionalDirectories => "additional_directories",
            Self::Args => "args",
            Self::Capabilities => "capabilities",
            Self::SkipPermissions => "skip_permissions",
            Self::SkipPermissionsArgs => "skip_permissions_args",
            Self::SkipPermissionsSessionMode => "skip_permissions_session_mode",
            Self::ProfileSelect => "profile.select",
            Self::ProfileMaterialize => "profile.materialize",
        }
    }

    fn is_text(self) -> bool {
        !matches!(self, Self::SkipPermissions | Self::ProfileMaterialize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMaterializeDraft {
    Default,
    True,
    False,
}

impl ProfileMaterializeDraft {
    fn from_config(value: Option<bool>) -> Self {
        match value {
            Some(true) => Self::True,
            Some(false) => Self::False,
            None => Self::Default,
        }
    }

    fn to_config(self) -> Option<bool> {
        match self {
            Self::Default => None,
            Self::True => Some(true),
            Self::False => Some(false),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::True => "true",
            Self::False => "false",
        }
    }

    fn cycle(self) -> Self {
        match self {
            Self::Default => Self::True,
            Self::True => Self::False,
            Self::False => Self::Default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftValidationError {
    pub field: EditableAgentField,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfigDraft {
    additional_directories: String,
    args: String,
    capabilities: String,
    skip_permissions: bool,
    skip_permissions_args: String,
    skip_permissions_session_mode: String,
    profile_select: String,
    profile_materialize: ProfileMaterializeDraft,
}

impl AgentConfigDraft {
    pub fn from_config(config: &AgentConfig) -> Self {
        Self {
            additional_directories: join_paths(&config.additional_directories),
            args: join_strings(&config.args),
            capabilities: join_strings(&config.capabilities),
            skip_permissions: config.skip_permissions,
            skip_permissions_args: join_strings(&config.skip_permissions_args),
            skip_permissions_session_mode: config
                .skip_permissions_session_mode
                .clone()
                .unwrap_or_default(),
            profile_select: config
                .profile
                .as_ref()
                .and_then(|profile| profile.select.clone())
                .unwrap_or_default(),
            profile_materialize: ProfileMaterializeDraft::from_config(
                config
                    .profile
                    .as_ref()
                    .and_then(|profile| profile.materialize),
            ),
        }
    }

    pub fn set_text(&mut self, field: EditableAgentField, value: impl Into<String>) {
        let value = value.into();
        match field {
            EditableAgentField::AdditionalDirectories => self.additional_directories = value,
            EditableAgentField::Args => self.args = value,
            EditableAgentField::Capabilities => self.capabilities = value,
            EditableAgentField::SkipPermissionsArgs => self.skip_permissions_args = value,
            EditableAgentField::SkipPermissionsSessionMode => {
                self.skip_permissions_session_mode = value;
            }
            EditableAgentField::ProfileSelect => self.profile_select = value,
            EditableAgentField::SkipPermissions | EditableAgentField::ProfileMaterialize => {}
        }
    }

    pub fn set_bool(&mut self, field: EditableAgentField, value: bool) {
        if matches!(field, EditableAgentField::SkipPermissions) {
            self.skip_permissions = value;
        }
    }

    pub fn set_materialize(&mut self, value: ProfileMaterializeDraft) {
        self.profile_materialize = value;
    }

    fn text(&self, field: EditableAgentField) -> String {
        match field {
            EditableAgentField::AdditionalDirectories => self.additional_directories.clone(),
            EditableAgentField::Args => self.args.clone(),
            EditableAgentField::Capabilities => self.capabilities.clone(),
            EditableAgentField::SkipPermissions => self.skip_permissions.to_string(),
            EditableAgentField::SkipPermissionsArgs => self.skip_permissions_args.clone(),
            EditableAgentField::SkipPermissionsSessionMode => {
                self.skip_permissions_session_mode.clone()
            }
            EditableAgentField::ProfileSelect => self.profile_select.clone(),
            EditableAgentField::ProfileMaterialize => self.profile_materialize.label().to_string(),
        }
    }

    pub fn to_updated_entry(
        &self,
        base: &AgentConfig,
    ) -> Result<AgentConfig, DraftValidationError> {
        let mut updated = base.clone();
        updated.additional_directories = parse_path_list(&self.additional_directories)?;
        updated.args = parse_string_list(&self.args);
        updated.capabilities = parse_string_list(&self.capabilities);
        updated.skip_permissions = self.skip_permissions;
        updated.skip_permissions_args = parse_string_list(&self.skip_permissions_args);
        updated.skip_permissions_session_mode =
            optional_trimmed(&self.skip_permissions_session_mode);

        let select = optional_trimmed(&self.profile_select);
        let materialize = self.profile_materialize.to_config();
        updated.profile = if select.is_none() && materialize.is_none() {
            None
        } else {
            Some(ProfileConfig {
                select,
                materialize,
            })
        };

        Ok(updated)
    }
}

pub struct AgentConfigBrowserView {
    entries: Vec<AgentConfig>,
    selected_agent: usize,
    selected_field: usize,
    selected_section: usize,
    pane: BrowserPane,
    section: ConfigureSection,
    graph_pane: GraphPane,
    tui_pane: TuiPane,
    skills_pane: SkillsPane,
    draft: Option<AgentConfigDraft>,
    edit_buffer: Option<String>,
    validation_error: Option<DraftValidationError>,
}

impl AgentConfigBrowserView {
    pub fn new(entries: Vec<AgentConfig>, preselect: Option<String>) -> Self {
        let mut view = Self {
            entries,
            selected_agent: 0,
            selected_field: 0,
            selected_section: 0,
            pane: BrowserPane::Agents,
            section: ConfigureSection::Agents,
            graph_pane: GraphPane::new(None, Default::default()),
            tui_pane: TuiPane::new(),
            skills_pane: SkillsPane::new(),
            draft: None,
            edit_buffer: None,
            validation_error: None,
        };
        view.apply_configure_arg(preselect.as_deref().unwrap_or(""));
        view.rebuild_draft();
        view
    }

    pub fn set_entries(&mut self, entries: Vec<AgentConfig>, preselect: Option<String>) {
        let previous_name = self.selected_agent_name().map(str::to_string);
        self.entries = entries;
        self.selected_agent = 0;
        if let Some(preselect) = preselect.as_deref() {
            self.apply_configure_arg(preselect);
        } else if let Some(previous_name) = previous_name {
            self.apply_preselect(Some(&previous_name));
        }
        self.rebuild_draft();
    }

    pub fn set_graph_config(&mut self, graph: &GraphConfig) {
        self.graph_pane = GraphPane::new(graph.embedding_model.as_deref(), graph.overlay_fsmonitor);
    }

    pub fn replace_agent_config(&mut self, name: &str, updated: AgentConfig) {
        if let Some(slot) = self.entries.iter_mut().find(|entry| entry.name == name) {
            *slot = updated;
        }
        if self.selected_agent_name() == Some(name) {
            self.rebuild_draft();
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn selected_agent_name_for_test(&self) -> Option<&str> {
        self.selected_agent_name()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn section_for_test(&self) -> ConfigureSection {
        self.section
    }

    #[cfg(any(test, debug_assertions))]
    pub fn selected_field_index_for_test(&self) -> usize {
        self.selected_field
    }

    pub fn editing_active(&self) -> bool {
        self.edit_buffer.is_some()
    }

    fn selected_agent_name(&self) -> Option<&str> {
        self.entries
            .get(self.selected_agent)
            .map(|entry| entry.name.as_str())
    }

    fn selected_entry(&self) -> Option<&AgentConfig> {
        self.entries.get(self.selected_agent)
    }

    fn selected_field(&self) -> EditableAgentField {
        FIELD_ORDER[self.selected_field]
    }

    fn apply_preselect(&mut self, preselect: Option<&str>) {
        if let Some(preselect) = preselect {
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.name == preselect.trim())
            {
                self.selected_agent = index;
            }
        }
    }

    fn apply_configure_arg(&mut self, raw: &str) {
        let (section, agent) = parse_configure_arg(raw);
        self.section = section;
        self.selected_section = ConfigureSection::ALL
            .iter()
            .position(|candidate| *candidate == section)
            .unwrap_or(0);
        self.pane = match section {
            ConfigureSection::Agents => BrowserPane::Agents,
            _ => BrowserPane::Fields,
        };
        self.apply_preselect(agent.as_deref());
    }

    fn move_section(&mut self, delta: isize) {
        self.selected_section =
            offset_index(self.selected_section, ConfigureSection::ALL.len(), delta);
    }

    fn activate_selected_section(&mut self) {
        self.section = ConfigureSection::ALL[self.selected_section];
        self.pane = match self.section {
            ConfigureSection::Agents => BrowserPane::Agents,
            _ => BrowserPane::Fields,
        };
    }

    fn cycle_pane(&mut self) {
        if self.section == ConfigureSection::Agents {
            self.pane = match self.pane {
                BrowserPane::Sections => BrowserPane::Agents,
                BrowserPane::Agents => BrowserPane::Fields,
                BrowserPane::Fields => BrowserPane::Sections,
            };
        } else {
            self.pane = match self.pane {
                BrowserPane::Sections => BrowserPane::Fields,
                _ => BrowserPane::Sections,
            };
        }
    }

    fn handle_active_section_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.section {
            ConfigureSection::Agents => None,
            ConfigureSection::Graph => self.graph_pane.handle_key(key),
            ConfigureSection::Tui => self.tui_pane.handle_key(key),
            ConfigureSection::Skills => self.skills_pane.handle_key(key),
        }
    }

    fn agents_section_active(&self) -> bool {
        self.section == ConfigureSection::Agents
    }

    fn rebuild_draft(&mut self) {
        self.draft = self.selected_entry().map(AgentConfigDraft::from_config);
        self.edit_buffer = None;
        self.validation_error = None;
    }

    fn move_agent(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        self.selected_agent = offset_index(self.selected_agent, self.entries.len(), delta);
        self.rebuild_draft();
    }

    fn move_field(&mut self, delta: isize) {
        self.selected_field = offset_index(self.selected_field, FIELD_ORDER.len(), delta);
        self.edit_buffer = None;
        self.validation_error = None;
    }

    fn start_text_edit(&mut self) {
        let field = self.selected_field();
        if field.is_text() {
            if let Some(draft) = self.draft.as_ref() {
                self.edit_buffer = Some(draft.text(field));
            }
        }
    }

    fn commit_text_edit(&mut self) {
        if let Some(value) = self.edit_buffer.take() {
            let field = self.selected_field();
            if let Some(draft) = self.draft.as_mut() {
                draft.set_text(field, value);
                self.validation_error = None;
            }
        }
    }

    fn toggle_current_field(&mut self) {
        match self.selected_field() {
            EditableAgentField::SkipPermissions => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.skip_permissions = !draft.skip_permissions;
                }
            }
            EditableAgentField::ProfileMaterialize => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.profile_materialize = draft.profile_materialize.cycle();
                }
            }
            _ => self.start_text_edit(),
        }
        self.validation_error = None;
    }

    fn save_action(&mut self) -> Option<Action> {
        self.commit_text_edit();
        let base = self.selected_entry()?.clone();
        let draft = self.draft.as_ref()?;
        match draft.to_updated_entry(&base) {
            Ok(updated_entry) => {
                let name = updated_entry.name.clone();
                self.replace_agent_config(&name, updated_entry.clone());
                Some(Action::AgentConfigSaveRequested {
                    name,
                    updated_entry,
                })
            }
            Err(err) => {
                self.validation_error = Some(err);
                None
            }
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.edit_buffer = None;
                self.validation_error = None;
            }
            KeyCode::Enter => self.commit_text_edit(),
            KeyCode::Backspace => {
                if let Some(buffer) = self.edit_buffer.as_mut() {
                    buffer.pop();
                }
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(buffer) = self.edit_buffer.as_mut() {
                    buffer.push(ch);
                }
            }
            _ => {}
        }
        None
    }

    fn render_inner(&self, frame: &mut Frame, area: Rect) {
        let panes = split_configure(area, self.agents_section_active());
        self.render_sections(frame, panes.sections);
        if self.agents_section_active() {
            if let Some(agents) = panes.agents {
                self.render_agents(frame, agents);
            }
            self.render_fields(frame, panes.detail);
        } else {
            match self.section {
                ConfigureSection::Graph => self.graph_pane.render(frame, panes.detail),
                ConfigureSection::Tui => self.tui_pane.render(frame, panes.detail),
                ConfigureSection::Skills => self.skills_pane.render(frame, panes.detail),
                ConfigureSection::Agents => {}
            }
        }
        self.render_status(frame, panes.status);
    }

    fn render_sections(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.pane == BrowserPane::Sections {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let block = Block::default()
            .title("Sections")
            .borders(Borders::ALL)
            .border_style(border_style);
        let lines: Vec<Line> = ConfigureSection::ALL
            .iter()
            .enumerate()
            .map(|(index, section)| {
                let marker = if index == self.selected_section {
                    "> "
                } else {
                    "  "
                };
                let style = if index == self.selected_section {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::raw(marker),
                    Span::styled(section.list_label(), style),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_agents(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.pane == BrowserPane::Agents {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let block = Block::default()
            .title("Agents")
            .borders(Borders::ALL)
            .border_style(border_style);
        let lines = if self.entries.is_empty() {
            vec![Line::from("No configured agents")]
        } else {
            self.entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let marker = if index == self.selected_agent {
                        "> "
                    } else {
                        "  "
                    };
                    let style = if index == self.selected_agent {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    Line::from(vec![
                        Span::raw(marker),
                        Span::styled(entry.name.clone(), style),
                    ])
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_fields(&self, frame: &mut Frame, area: Rect) {
        let title = self
            .selected_agent_name()
            .map(|name| format!("Settings: {name}"))
            .unwrap_or_else(|| "Settings".to_string());
        let border_style = if self.pane == BrowserPane::Fields {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);

        let mut lines = Vec::new();
        if let Some(draft) = self.draft.as_ref() {
            for (index, field) in FIELD_ORDER.iter().enumerate() {
                let selected = index == self.selected_field;
                let value = if selected {
                    self.edit_buffer
                        .clone()
                        .unwrap_or_else(|| draft.text(*field))
                } else {
                    draft.text(*field)
                };
                let marker = if selected { "> " } else { "  " };
                let value_style = if selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(field.label(), Style::default().fg(Color::Gray)),
                    Span::raw(": "),
                    Span::styled(display_blank(&value), value_style),
                ]));
                if let Some(error) = &self.validation_error {
                    if selected && error.field == *field {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", error.message),
                            Style::default().fg(ERROR_FG),
                        )));
                    }
                }
            }
        } else {
            lines.push(Line::from("No agent selected"));
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let status = if self.edit_buffer.is_some() {
            STATUS_EDITING
        } else {
            STATUS_IDLE
        };
        frame.render_widget(Paragraph::new(status), area);
    }
}

struct ConfigurePanes {
    sections: Rect,
    agents: Option<Rect>,
    detail: Rect,
    status: Rect,
}

fn split_configure(area: Rect, agents_section: bool) -> ConfigurePanes {
    let chunks =
        Layout::vertical([Constraint::Min(3), Constraint::Length(STATUS_HEIGHT)]).split(area);
    let body = chunks[0];
    let status = chunks[1];
    if agents_section {
        if body.width >= THREE_PANE_MIN_WIDTH {
            let cols = Layout::horizontal([
                Constraint::Length(MIN_SECTIONS_WIDTH),
                Constraint::Length(MIN_AGENTS_WIDTH),
                Constraint::Min(MIN_FIELDS_WIDTH),
            ])
            .spacing(GUTTER)
            .split(body);
            ConfigurePanes {
                sections: cols[0],
                agents: Some(cols[1]),
                detail: cols[2],
                status,
            }
        } else {
            let rows = Layout::vertical([
                Constraint::Length(SECTIONS_STACK_HEIGHT),
                Constraint::Min(6),
                Constraint::Min(10),
            ])
            .split(body);
            ConfigurePanes {
                sections: rows[0],
                agents: Some(rows[1]),
                detail: rows[2],
                status,
            }
        }
    } else if body.width >= TWO_PANE_MIN_WIDTH {
        let cols = Layout::horizontal([
            Constraint::Length(MIN_SECTIONS_WIDTH),
            Constraint::Min(MIN_AGENTS_WIDTH),
        ])
        .spacing(GUTTER)
        .split(body);
        ConfigurePanes {
            sections: cols[0],
            agents: None,
            detail: cols[1],
            status,
        }
    } else {
        let rows = Layout::vertical([
            Constraint::Length(SECTIONS_STACK_HEIGHT),
            Constraint::Min(10),
        ])
        .split(body);
        ConfigurePanes {
            sections: rows[0],
            agents: None,
            detail: rows[1],
            status,
        }
    }
}

impl View for AgentConfigBrowserView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
        if self.edit_buffer.is_some() {
            return self.handle_editing_key(key);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::NavigateBack),
            KeyCode::Tab => {
                self.cycle_pane();
                None
            }
            KeyCode::Right if self.pane == BrowserPane::Sections => {
                self.activate_selected_section();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.pane == BrowserPane::Sections {
                    self.move_section(1);
                    None
                } else if !self.agents_section_active() {
                    self.handle_active_section_key(key)
                } else if self.pane == BrowserPane::Agents {
                    self.move_agent(1);
                    None
                } else {
                    self.move_field(1);
                    None
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.pane == BrowserPane::Sections {
                    self.move_section(-1);
                    None
                } else if !self.agents_section_active() {
                    self.handle_active_section_key(key)
                } else if self.pane == BrowserPane::Agents {
                    self.move_agent(-1);
                    None
                } else {
                    self.move_field(-1);
                    None
                }
            }
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char(' ') => {
                if self.pane == BrowserPane::Sections {
                    self.activate_selected_section();
                    None
                } else if !self.agents_section_active() {
                    self.handle_active_section_key(key)
                } else if self.pane == BrowserPane::Agents {
                    self.pane = BrowserPane::Fields;
                    None
                } else {
                    self.toggle_current_field();
                    None
                }
            }
            KeyCode::Char('s') => {
                if self.pane == BrowserPane::Sections {
                    None
                } else if !self.agents_section_active() {
                    self.handle_active_section_key(key)
                } else {
                    self.save_action()
                }
            }
            _ if self.pane != BrowserPane::Sections && !self.agents_section_active() => {
                self.handle_active_section_key(key)
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &ViewContext) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
        self.render_inner(frame, area);
    }

    fn tick(&mut self) {}
}

fn parse_path_list(value: &str) -> Result<Vec<PathBuf>, DraftValidationError> {
    parse_string_list(value)
        .into_iter()
        .map(|entry| {
            let path = PathBuf::from(&entry);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(DraftValidationError {
                    field: EditableAgentField::AdditionalDirectories,
                    message: format!(
                        "additional_directories entries must be absolute paths: {entry}"
                    ),
                })
            }
        })
        .collect()
}

fn parse_string_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn optional_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn join_strings(values: &[String]) -> String {
    values.join(",")
}

fn join_paths(values: &[PathBuf]) -> String {
    values
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn display_blank(value: &str) -> String {
    if value.is_empty() {
        "<default>".to_string()
    } else {
        value.to_string()
    }
}

fn offset_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, len.saturating_sub(1) as isize) as usize
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use spur_acp::{AgentConfig, ProfileConfig};

    use super::*;

    fn configured_agent() -> AgentConfig {
        let mut config = AgentConfig::with_defaults("codex");
        config.command = "codex".into();
        config.args = vec!["exec".into()];
        config.capabilities = vec!["rust".into()];
        config.skip_permissions = false;
        config
    }

    #[test]
    fn draft_save_rejects_relative_additional_directories() {
        let base = configured_agent();
        let mut draft = AgentConfigDraft::from_config(&base);
        draft.set_text(
            EditableAgentField::AdditionalDirectories,
            "/abs/path,relative/path",
        );

        let err = draft.to_updated_entry(&base).unwrap_err();

        assert_eq!(err.field, EditableAgentField::AdditionalDirectories);
        assert!(err.message.contains("relative/path"));
        assert!(err.message.contains("absolute"));
    }

    #[test]
    fn draft_save_updates_only_curated_fields() {
        let base = configured_agent();
        let mut draft = AgentConfigDraft::from_config(&base);
        draft.set_text(EditableAgentField::Args, "exec,--model,gpt-5");
        draft.set_text(EditableAgentField::Capabilities, "rust,tests,review");
        draft.set_bool(EditableAgentField::SkipPermissions, true);
        draft.set_text(EditableAgentField::SkipPermissionsArgs, "--danger");
        draft.set_text(
            EditableAgentField::SkipPermissionsSessionMode,
            "bypassPermissions",
        );
        draft.set_text(EditableAgentField::ProfileSelect, "session_mode");
        draft.set_materialize(ProfileMaterializeDraft::True);
        draft.set_text(EditableAgentField::AdditionalDirectories, "/tmp,/var/tmp");

        let updated = draft.to_updated_entry(&base).unwrap();

        assert_eq!(updated.name, "codex");
        assert_eq!(updated.command, "codex");
        assert_eq!(updated.transport, base.transport);
        assert_eq!(updated.kind, base.kind);
        assert_eq!(updated.args, vec!["exec", "--model", "gpt-5"]);
        assert_eq!(updated.capabilities, vec!["rust", "tests", "review"]);
        assert_eq!(
            updated.additional_directories,
            vec![PathBuf::from("/tmp"), PathBuf::from("/var/tmp")]
        );
        assert!(updated.skip_permissions);
        assert_eq!(updated.skip_permissions_args, vec!["--danger"]);
        assert_eq!(
            updated.skip_permissions_session_mode.as_deref(),
            Some("bypassPermissions")
        );
        assert_eq!(
            updated.profile,
            Some(ProfileConfig {
                select: Some("session_mode".into()),
                materialize: Some(true),
            })
        );
    }

    #[test]
    fn blank_profile_override_collapses_to_none() {
        let mut base = configured_agent();
        base.profile = Some(ProfileConfig {
            select: Some("session_mode".into()),
            materialize: Some(true),
        });
        let mut draft = AgentConfigDraft::from_config(&base);
        draft.set_text(EditableAgentField::ProfileSelect, "");
        draft.set_materialize(ProfileMaterializeDraft::Default);

        let updated = draft.to_updated_entry(&base).unwrap();

        assert_eq!(updated.profile, None);
    }

    #[test]
    fn preselect_focuses_named_agent() {
        let mut second = configured_agent();
        second.name = "kiro".into();

        let view =
            AgentConfigBrowserView::new(vec![configured_agent(), second], Some("kiro".into()));

        assert_eq!(view.selected_agent_name_for_test(), Some("kiro"));
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn test_ctx(lineage: &spur_core::ExecutorLineage) -> ViewContext<'_> {
        ViewContext::test_ctx(lineage)
    }

    #[test]
    fn graph_token_focuses_graph_section() {
        let view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
        assert_eq!(
            view.section_for_test(),
            crate::configure_section::ConfigureSection::Graph
        );
    }

    #[test]
    fn agent_token_still_preselects_agent() {
        let mut second = configured_agent();
        second.name = "kiro".into();
        let view =
            AgentConfigBrowserView::new(vec![configured_agent(), second], Some("kiro".into()));
        assert_eq!(
            view.section_for_test(),
            crate::configure_section::ConfigureSection::Agents
        );
        assert_eq!(view.selected_agent_name_for_test(), Some("kiro"));
    }

    #[test]
    fn graph_section_down_does_not_move_agent_field() {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
        let field_before = view.selected_field_index_for_test();

        view.handle_key(key(KeyCode::Down), &ctx);

        assert_eq!(view.selected_field_index_for_test(), field_before);
        assert_eq!(
            view.section_for_test(),
            crate::configure_section::ConfigureSection::Graph
        );
        assert!(!view.editing_active());
    }

    #[test]
    fn graph_section_keys_go_to_pane_not_agent_draft() {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
        let field_before = view.selected_field_index_for_test();
        let agent_before = view.selected_agent_name_for_test().map(str::to_string);

        for code in [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('s'),
            KeyCode::Char('x'),
        ] {
            let action = view.handle_key(key(code), &ctx);
            assert!(
                !matches!(action, Some(Action::AgentConfigSaveRequested { .. })),
                "graph pane must not save agent config on {code:?}"
            );
        }

        assert_eq!(view.selected_field_index_for_test(), field_before);
        assert_eq!(view.selected_agent_name_for_test(), agent_before.as_deref());
        assert!(!view.editing_active());
    }

    #[test]
    fn graph_pane_refreshes_from_confirmed_config() {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
        let graph = spur_acp::config::GraphConfig {
            embedding_model: Some("coderank".into()),
            overlay_fsmonitor: spur_acp::config::OverlayFsmonitorMode::Auto,
        };

        view.set_graph_config(&graph);

        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s')), &ctx),
            Some(Action::ConfigSaveRequested {
                patch: spur_acp::config::ConfigPatch::GraphEmbeddingModel { ref alias }
            }) if alias == "coderank"
        ));
        view.handle_key(key(KeyCode::Down), &ctx);
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s')), &ctx),
            Some(Action::ConfigSaveRequested {
                patch: spur_acp::config::ConfigPatch::GraphOverlayFsmonitor(
                    spur_acp::config::OverlayFsmonitorMode::Auto
                )
            })
        ));
    }

    #[test]
    fn enter_on_sections_activates_highlighted_section() {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], None);

        view.handle_key(key(KeyCode::Tab), &ctx);
        view.handle_key(key(KeyCode::Tab), &ctx);
        view.handle_key(key(KeyCode::Down), &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);

        assert_eq!(
            view.section_for_test(),
            crate::configure_section::ConfigureSection::Graph
        );
        let field_before = view.selected_field_index_for_test();
        view.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(view.selected_field_index_for_test(), field_before);
        assert!(!view.editing_active());
    }

    #[test]
    fn agents_tab_then_down_still_moves_fields() {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], None);

        view.handle_key(key(KeyCode::Tab), &ctx);
        view.handle_key(key(KeyCode::Down), &ctx);

        assert_eq!(
            view.section_for_test(),
            crate::configure_section::ConfigureSection::Agents
        );
        assert_eq!(view.selected_field_index_for_test(), 1);
    }

    #[test]
    fn status_chrome_fits_narrow_terminal() {
        assert!(STATUS_IDLE.len() <= STATUS_MAX_COLS);
        assert!(STATUS_EDITING.len() <= STATUS_MAX_COLS);
    }

    #[test]
    fn three_pane_at_80_uses_solver_mins_and_gutters() {
        let panes = split_configure(Rect::new(0, 0, 80, 24), true);
        let agents = panes.agents.expect("agents column");
        assert_eq!(panes.sections.width, MIN_SECTIONS_WIDTH);
        assert_eq!(agents.width, MIN_AGENTS_WIDTH);
        assert!(panes.detail.width >= MIN_FIELDS_WIDTH);
        assert_eq!(agents.x, panes.sections.x + panes.sections.width + GUTTER);
        assert_eq!(panes.detail.x, agents.x + agents.width + GUTTER);
        assert_eq!(
            panes.sections.width + GUTTER + agents.width + GUTTER + panes.detail.width,
            80
        );
        assert_eq!(panes.status.height, STATUS_HEIGHT);
        assert_eq!(panes.status.y, 23);
    }

    #[test]
    fn three_pane_below_floor_stacks_vertically() {
        let panes = split_configure(Rect::new(0, 0, 40, 24), true);
        let agents = panes.agents.expect("agents row");
        assert_eq!(panes.sections.width, 40);
        assert_eq!(agents.width, 40);
        assert_eq!(panes.detail.width, 40);
        assert!(panes.sections.y < agents.y);
        assert!(agents.y < panes.detail.y);
        assert_eq!(panes.detail.x, panes.sections.x);
        assert!(panes.detail.height >= 8);
    }

    #[test]
    fn two_pane_at_80_keeps_section_floor_and_gutter() {
        let panes = split_configure(Rect::new(0, 0, 80, 24), false);
        assert!(panes.agents.is_none());
        assert_eq!(panes.sections.width, MIN_SECTIONS_WIDTH);
        assert_eq!(
            panes.detail.x,
            panes.sections.x + panes.sections.width + GUTTER
        );
        assert_eq!(panes.sections.width + GUTTER + panes.detail.width, 80);
    }

    fn rendered_configure(view: &mut AgentConfigBrowserView, width: u16, height: u16) -> String {
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, frame.area(), &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        rendered
    }

    #[test]
    fn render_shows_title_case_pane_headers() {
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
        let text = rendered_configure(&mut view, 80, 24);
        assert!(text.contains("Graph"), "{text}");
        assert!(text.contains("Sections"), "{text}");
    }

    #[test]
    fn validation_error_uses_light_red() {
        let mut view = AgentConfigBrowserView::new(vec![configured_agent()], None);
        view.pane = BrowserPane::Fields;
        view.selected_field = 1;
        view.validation_error = Some(DraftValidationError {
            field: EditableAgentField::Args,
            message: "bad-args".to_string(),
        });
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = test_ctx(&lineage);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| view.render(frame, frame.area(), &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut saw_light_red = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].style().fg == Some(ERROR_FG) {
                    saw_light_red = true;
                }
            }
        }
        assert!(saw_light_red, "expected LightRed error cells");
    }
}
