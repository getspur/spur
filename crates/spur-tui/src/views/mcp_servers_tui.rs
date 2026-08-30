use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::config::{ConfigPatch, McpServerEntry, McpServerTransport, McpServersConfig};

use crate::action::Action;

const NEXT_SESSION_NOTICE: &str = "MCP config applies to next session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    None,
    Stdio,
    Http,
}

impl Form {
    fn transport_label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }

    fn field_count(self) -> usize {
        match self {
            Self::None => 0,
            Self::Stdio => 6,
            Self::Http => 5,
        }
    }
}

#[derive(Debug, Clone)]
struct McpServerDraft {
    name: String,
    enabled: bool,
    command: String,
    args: String,
    env: String,
    url: String,
    headers: String,
}

impl Default for McpServerDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            command: String::new(),
            args: String::new(),
            env: String::new(),
            url: String::new(),
            headers: String::new(),
        }
    }
}

impl McpServerDraft {
    fn from_entry(entry: &McpServerEntry) -> Self {
        let mut draft = Self {
            name: entry.name.clone(),
            enabled: entry.enabled,
            ..Self::default()
        };
        match &entry.transport {
            McpServerTransport::Stdio { command, args, env } => {
                draft.command.clone_from(command);
                draft.args = args.join(",");
                draft.env = format_pairs(env);
            }
            McpServerTransport::Http { url, headers } => {
                draft.url.clone_from(url);
                draft.headers = format_pairs(headers);
            }
        }
        draft
    }

    fn into_entry(self, form: Form) -> McpServerEntry {
        let transport = match form {
            Form::Stdio => McpServerTransport::Stdio {
                command: self.command,
                args: parse_list(&self.args),
                env: parse_pairs(&self.env),
            },
            Form::Http => McpServerTransport::Http {
                url: self.url,
                headers: parse_pairs(&self.headers),
            },
            Form::None => unreachable!("a draft is saved only from an active form"),
        };
        McpServerEntry {
            name: self.name.trim().to_string(),
            enabled: self.enabled,
            transport,
        }
    }
}

/// `/configure mcp` list and editor pane.
pub struct McpServersPane {
    entries: Vec<McpServerEntry>,
    selected_entry: usize,
    form: Form,
    draft: McpServerDraft,
    selected_field: usize,
    edit_buffer: Option<String>,
    editing_existing: bool,
    error: Option<String>,
}

impl McpServersPane {
    /// Creates an empty pane; live config is supplied by [`Self::set_mcp_config`].
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected_entry: 0,
            form: Form::None,
            draft: McpServerDraft::default(),
            selected_field: 0,
            edit_buffer: None,
            editing_existing: false,
            error: None,
        }
    }

    /// Replaces the displayed entries while preserving selection by name.
    pub fn set_entries(&mut self, entries: Vec<McpServerEntry>) {
        let selected_name = self
            .entries
            .get(self.selected_entry)
            .map(|entry| entry.name.clone());
        self.entries = entries;
        self.selected_entry = selected_name
            .and_then(|name| self.entries.iter().position(|entry| entry.name == name))
            .unwrap_or_else(|| {
                self.selected_entry
                    .min(self.entries.len().saturating_sub(1))
            });
    }

    /// Loads the confirmed MCP config snapshot.
    pub fn set_mcp_config(&mut self, config: &McpServersConfig) {
        self.set_entries(config.entries.clone());
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let regions = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let lines = self
            .content_rows()
            .into_iter()
            .map(Line::from)
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("MCP Servers")),
            regions[0],
        );
        frame.render_widget(Paragraph::new(NEXT_SESSION_NOTICE), regions[1]);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.edit_buffer.is_some() {
            return self.handle_editing_key(key);
        }
        match self.form {
            Form::None => self.handle_list_key(key),
            Form::Stdio | Form::Http => self.handle_form_key(key),
        }
    }

    pub fn form_active(&self) -> bool {
        self.form != Form::None
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_entry(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_entry(1);
                None
            }
            KeyCode::Char('a') => {
                self.begin_add();
                None
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.begin_edit();
                None
            }
            KeyCode::Char(' ') => self.toggle_selected_action(),
            KeyCode::Delete | KeyCode::Char('d') => self.remove_selected_action(),
            _ => None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.close_form();
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_field(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_field(1);
                None
            }
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char(' ') => {
                self.activate_form_field();
                None
            }
            KeyCode::Char('s') => self.save_form(),
            _ => None,
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.edit_buffer = None;
            }
            KeyCode::Enter => self.commit_field_edit(),
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

    fn begin_add(&mut self) {
        self.form = Form::Stdio;
        self.draft = McpServerDraft::default();
        self.selected_field = 0;
        self.edit_buffer = None;
        self.editing_existing = false;
        self.error = None;
    }

    fn begin_edit(&mut self) {
        let Some(entry) = self.entries.get(self.selected_entry).cloned() else {
            return;
        };
        self.form = match &entry.transport {
            McpServerTransport::Stdio { .. } => Form::Stdio,
            McpServerTransport::Http { .. } => Form::Http,
        };
        self.draft = McpServerDraft::from_entry(&entry);
        self.selected_field = 0;
        self.edit_buffer = None;
        self.editing_existing = true;
        self.error = None;
    }

    fn close_form(&mut self) {
        self.form = Form::None;
        self.selected_field = 0;
        self.edit_buffer = None;
        self.editing_existing = false;
        self.error = None;
    }

    fn move_entry(&mut self, delta: isize) {
        if !self.entries.is_empty() {
            self.selected_entry = offset_index(self.selected_entry, self.entries.len(), delta);
        }
    }

    fn move_field(&mut self, delta: isize) {
        self.selected_field = offset_index(self.selected_field, self.form.field_count(), delta);
        self.error = None;
    }

    fn activate_form_field(&mut self) {
        match self.selected_field {
            1 => self.draft.enabled = !self.draft.enabled,
            2 => {
                self.form = match self.form {
                    Form::Stdio => Form::Http,
                    Form::Http => Form::Stdio,
                    Form::None => Form::None,
                };
            }
            _ => self.edit_buffer = Some(self.field_value(self.selected_field)),
        }
        self.error = None;
    }

    fn commit_field_edit(&mut self) {
        let Some(value) = self.edit_buffer.take() else {
            return;
        };
        match (self.form, self.selected_field) {
            (_, 0) => self.draft.name = value,
            (Form::Stdio, 3) => self.draft.command = value,
            (Form::Stdio, 4) => self.draft.args = value,
            (Form::Stdio, 5) => self.draft.env = value,
            (Form::Http, 3) => self.draft.url = value,
            (Form::Http, 4) => self.draft.headers = value,
            _ => {}
        }
    }

    fn field_value(&self, index: usize) -> String {
        match (self.form, index) {
            (_, 0) => self.draft.name.clone(),
            (_, 1) => self.draft.enabled.to_string(),
            (_, 2) => self.form.transport_label().to_string(),
            (Form::Stdio, 3) => self.draft.command.clone(),
            (Form::Stdio, 4) => self.draft.args.clone(),
            (Form::Stdio, 5) => self.draft.env.clone(),
            (Form::Http, 3) => self.draft.url.clone(),
            (Form::Http, 4) => self.draft.headers.clone(),
            _ => String::new(),
        }
    }

    fn save_form(&mut self) -> Option<Action> {
        if self.draft.name.trim().is_empty() {
            self.error = Some("name must not be empty".into());
            return None;
        }
        let entry = self.draft.clone().into_entry(self.form);
        let action = self.save_action_for(entry);
        self.close_form();
        Some(action)
    }

    fn toggle_selected_action(&self) -> Option<Action> {
        let mut entry = self.entries.get(self.selected_entry)?.clone();
        entry.enabled = !entry.enabled;
        Some(self.save_action_for(entry))
    }

    fn remove_selected_action(&self) -> Option<Action> {
        let name = self.entries.get(self.selected_entry)?.name.clone();
        Some(Action::ConfigSaveRequested {
            patch: ConfigPatch::McpServerRemove { name },
        })
    }

    fn save_action_for(&self, entry: McpServerEntry) -> Action {
        Action::ConfigSaveRequested {
            patch: ConfigPatch::McpServerUpsert { entry },
        }
    }

    fn content_rows(&self) -> Vec<String> {
        if self.form == Form::None {
            self.list_rows()
        } else {
            self.form_rows()
        }
    }

    fn list_rows(&self) -> Vec<String> {
        let mut rows = vec!["a add  e edit  Space toggle  d remove".into()];
        if self.entries.is_empty() {
            rows.push("No configured MCP servers".into());
        } else {
            rows.extend(self.entries.iter().enumerate().map(|(index, entry)| {
                let marker = if index == self.selected_entry {
                    ">"
                } else {
                    " "
                };
                let transport = match &entry.transport {
                    McpServerTransport::Stdio { .. } => "stdio",
                    McpServerTransport::Http { .. } => "http",
                };
                let enabled = if entry.enabled { "enabled" } else { "disabled" };
                format!("{marker} {} [{transport}] {enabled}", entry.name)
            }));
        }
        rows
    }

    fn form_rows(&self) -> Vec<String> {
        let mut rows = vec![format!(
            "{} MCP server  Enter edit/toggle  s save  Esc cancel",
            if self.editing_existing { "Edit" } else { "Add" }
        )];
        let mut fields = vec![
            ("name", self.draft.name.clone()),
            ("enabled", self.draft.enabled.to_string()),
            ("transport", self.form.transport_label().to_string()),
        ];
        match self.form {
            Form::Stdio => fields.extend([
                ("command", self.draft.command.clone()),
                ("args", self.draft.args.clone()),
                ("env", self.draft.env.clone()),
            ]),
            Form::Http => fields.extend([
                ("url", self.draft.url.clone()),
                ("headers", self.draft.headers.clone()),
            ]),
            Form::None => {}
        }
        rows.extend(
            fields
                .into_iter()
                .enumerate()
                .map(|(index, (label, value))| {
                    let marker = if index == self.selected_field {
                        ">"
                    } else {
                        " "
                    };
                    let visible = if index == self.selected_field {
                        self.edit_buffer.as_deref().unwrap_or(&value)
                    } else {
                        &value
                    };
                    format!("{marker} {label}: {visible}")
                }),
        );
        if let Some(error) = &self.error {
            rows.push(format!("Error: {error}"));
        }
        rows
    }

    pub fn render_snapshot(&self) -> String {
        let mut rows = self.content_rows();
        rows.push(NEXT_SESSION_NOTICE.into());
        rows.join("\n")
    }
}

impl Default for McpServersPane {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_pairs(value: &str) -> HashMap<String, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (key.trim().to_string(), value.trim().to_string())
        })
        .collect()
}

fn format_pairs(pairs: &HashMap<String, String>) -> String {
    let mut pairs = pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs.join(",")
}

fn offset_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (index as isize + delta).rem_euclid(len as isize) as usize
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::config::{ConfigPatch, McpServerEntry, McpServerTransport, McpServersConfig};

    use crate::action::Action;

    use super::McpServersPane;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn enter_text(pane: &mut McpServersPane, value: &str) {
        assert!(pane.handle_key(key(KeyCode::Enter)).is_none());
        for ch in value.chars() {
            assert!(pane.handle_key(key(KeyCode::Char(ch))).is_none());
        }
        assert!(pane.handle_key(key(KeyCode::Enter)).is_none());
    }

    fn test_http_entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.into(),
            enabled: true,
            transport: McpServerTransport::Http {
                url: "https://example.test/mcp".into(),
                headers: HashMap::from([("Authorization".into(), "Bearer test".into())]),
            },
        }
    }

    fn disabled_entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            enabled: false,
            ..test_http_entry(name)
        }
    }

    #[test]
    fn save_emits_config_save_requested_upsert() {
        let entry = test_http_entry("github");
        let pane = McpServersPane::new();

        let action = pane.save_action_for(entry.clone());

        assert!(matches!(
            action,
            Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert { entry: saved }
            } if saved == entry
        ));
    }

    #[test]
    fn disabled_entry_renders_with_marker_and_next_session_notice() {
        let mut pane = McpServersPane::new();
        pane.set_entries(vec![disabled_entry("ghost")]);

        let text = pane.render_snapshot();

        assert!(text.contains("ghost") && text.contains("http"), "{text}");
        assert!(text.contains("disabled"), "{text}");
        assert!(text.contains("applies to next session"), "{text}");
    }

    #[test]
    fn list_toggle_and_remove_emit_existing_config_patches() {
        let mut pane = McpServersPane::new();
        pane.set_entries(vec![disabled_entry("ghost")]);

        assert!(matches!(
            pane.handle_key(key(KeyCode::Char(' '))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        enabled: true,
                        ..
                    }
                }
            }) if name == "ghost"
        ));
        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('d'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerRemove { ref name }
            }) if name == "ghost"
        ));
    }

    #[test]
    fn stdio_add_form_saves_command_args_and_env() {
        let mut pane = McpServersPane::new();
        pane.handle_key(key(KeyCode::Char('a')));
        enter_text(&mut pane, "local-tools");
        for _ in 0..3 {
            pane.handle_key(key(KeyCode::Down));
        }
        enter_text(&mut pane, "npx");
        pane.handle_key(key(KeyCode::Down));
        enter_text(&mut pane, "--yes,@example/mcp");
        pane.handle_key(key(KeyCode::Down));
        enter_text(&mut pane, "TOKEN=secret,MODE=dev");

        let action = pane.handle_key(key(KeyCode::Char('s')));

        assert!(matches!(
            action,
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        enabled: true,
                        transport: McpServerTransport::Stdio {
                            ref command,
                            ref args,
                            ref env,
                        },
                    }
                }
            }) if name == "local-tools"
                && command == "npx"
                && args == &["--yes", "@example/mcp"]
                && env.get("TOKEN").map(String::as_str) == Some("secret")
                && env.get("MODE").map(String::as_str) == Some("dev")
        ));
    }

    #[test]
    fn http_add_form_saves_url_and_headers() {
        let mut pane = McpServersPane::new();
        pane.handle_key(key(KeyCode::Char('a')));
        enter_text(&mut pane, "remote-tools");
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Enter));
        pane.handle_key(key(KeyCode::Down));
        enter_text(&mut pane, "https://example.test/mcp");
        pane.handle_key(key(KeyCode::Down));
        enter_text(&mut pane, "Authorization=Bearer test,X-Test=yes");

        let action = pane.handle_key(key(KeyCode::Char('s')));

        assert!(matches!(
            action,
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        transport: McpServerTransport::Http {
                            ref url,
                            ref headers,
                        },
                        ..
                    }
                }
            }) if name == "remote-tools"
                && url == "https://example.test/mcp"
                && headers.get("Authorization").map(String::as_str) == Some("Bearer test")
                && headers.get("X-Test").map(String::as_str) == Some("yes")
        ));
    }

    #[test]
    fn edit_loads_selected_entry_and_saves_an_upsert() {
        let entry = test_http_entry("github");
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig {
            entries: vec![entry.clone()],
        });

        pane.handle_key(key(KeyCode::Char('e')));

        let text = pane.render_snapshot();
        assert!(text.contains("transport: http"), "{text}");
        assert!(text.contains("https://example.test/mcp"), "{text}");
        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('s'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert { entry: saved }
            }) if saved == entry
        ));
    }

    #[test]
    fn blank_name_is_the_only_client_side_save_rejection() {
        let mut pane = McpServersPane::new();
        pane.handle_key(key(KeyCode::Char('a')));

        assert!(pane.handle_key(key(KeyCode::Char('s'))).is_none());
        assert!(pane.render_snapshot().contains("name must not be empty"));
    }
}
