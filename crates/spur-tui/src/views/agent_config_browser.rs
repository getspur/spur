use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::{AgentConfig, ProfileConfig, SpurEvent};

use crate::action::Action;

use super::{View, ViewContext};

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
    pane: BrowserPane,
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
            pane: BrowserPane::Agents,
            draft: None,
            edit_buffer: None,
            validation_error: None,
        };
        view.apply_preselect(preselect.as_deref());
        view.rebuild_draft();
        view
    }

    pub fn set_entries(&mut self, entries: Vec<AgentConfig>, preselect: Option<String>) {
        let previous_name = self.selected_agent_name().map(str::to_string);
        self.entries = entries;
        self.selected_agent = 0;
        if let Some(preselect) = preselect.as_deref() {
            self.apply_preselect(Some(preselect));
        } else if let Some(previous_name) = previous_name {
            self.apply_preselect(Some(&previous_name));
        }
        self.rebuild_draft();
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(chunks[0]);

        self.render_agents(frame, columns[0]);
        self.render_fields(frame, columns[1]);
        self.render_status(frame, chunks[1]);
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
                        .as_ref()
                        .cloned()
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
                            Style::default().fg(Color::Red),
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
            "Enter save field  Esc cancel"
        } else {
            "Tab pane  j/k move  Enter edit/toggle  s save  Esc back"
        };
        frame.render_widget(Paragraph::new(status), area);
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
                self.pane = match self.pane {
                    BrowserPane::Agents => BrowserPane::Fields,
                    BrowserPane::Fields => BrowserPane::Agents,
                };
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.pane == BrowserPane::Agents {
                    self.move_agent(1);
                } else {
                    self.move_field(1);
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.pane == BrowserPane::Agents {
                    self.move_agent(-1);
                } else {
                    self.move_field(-1);
                }
                None
            }
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char(' ') => {
                if self.pane == BrowserPane::Agents {
                    self.pane = BrowserPane::Fields;
                } else {
                    self.toggle_current_field();
                }
                None
            }
            KeyCode::Char('s') => self.save_action(),
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
}
