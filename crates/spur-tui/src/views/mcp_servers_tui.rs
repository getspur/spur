use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::config::{
    BuiltinMcpOverridesConfig, BuiltinMcpServer, ConfigPatch, McpServerEntry, McpServerTransport,
    McpServersConfig,
};
use spur_mcp::probe::{probe_server, ProbeOutcome, ProbeReport};
use tokio::task::JoinHandle;

use crate::{action::Action, views::builtin_confirm::BuiltinConfirmPane};

const NEXT_SESSION_NOTICE: &str = "MCP config applies to next session";
const BUILTIN_SERVERS: [BuiltinMcpServer; 3] = [
    BuiltinMcpServer::SpurMcp,
    BuiltinMcpServer::Notebook,
    BuiltinMcpServer::SpurWorkerMcp,
];

/// Future returned by an injectable MCP probe runner.
pub type ProbeFuture = Pin<Box<dyn Future<Output = ProbeReport> + Send + 'static>>;

/// Injectable runner used to keep pane tests independent of MCP I/O.
pub type ProbeHook = Arc<dyn Fn(McpServerEntry) -> ProbeFuture + Send + Sync + 'static>;

/// Resolved notebook executable and the nonce-specific control socket it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookMcpRuntimeSource {
    /// Resolved `SpurLab` or `spur-notebook` executable path.
    pub command: String,
    /// Control socket path passed to `--mcp-proxy`.
    pub socket_path: String,
}

/// Live worker MCP endpoint and its delegation-scoped bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerMcpRuntimeSource {
    /// Live worker MCP base URL.
    pub url: String,
    /// Token delivered through both the URL query and Authorization header.
    pub token: String,
}

/// Runtime-only sources used to describe and probe built-in MCP servers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinMcpRuntimeSources {
    /// Brain-facing `spur-mcp` callback URL, when visible to the pane owner.
    pub spur_mcp_url: Option<String>,
    /// Resolved notebook proxy launch data, when visible to the pane owner.
    pub notebook: Option<NotebookMcpRuntimeSource>,
    /// Live worker server data, when a worker MCP server is running.
    pub worker: Option<WorkerMcpRuntimeSource>,
}

/// Injectable snapshot provider for runtime-only built-in MCP endpoints.
pub type BuiltinSourceProvider = Arc<dyn Fn() -> BuiltinMcpRuntimeSources + Send + Sync + 'static>;

/// Awaitable pane task whose output carries the initiating server name.
pub type ProbeTask = JoinHandle<(String, ProbeReport)>;

#[derive(Debug, Clone, Default)]
enum ProbeState {
    #[default]
    Idle,
    Probing,
    Done(ProbeReport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    None,
    Stdio,
    Http,
    JuteDebug,
}

impl Form {
    fn transport_label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::JuteDebug => "jute-debug",
        }
    }

    fn field_count(self) -> usize {
        match self {
            Self::None => 0,
            Self::Stdio => 6,
            Self::Http => 5,
            Self::JuteDebug => 3,
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
            McpServerTransport::JuteDebug => {}
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
            Form::JuteDebug => McpServerTransport::JuteDebug,
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
    builtin_overrides: BuiltinMcpOverridesConfig,
    builtin_source_provider: BuiltinSourceProvider,
    builtin_confirm: BuiltinConfirmPane,
    probe_states: HashMap<String, ProbeState>,
    expanded_probe_entries: HashSet<String>,
    probe_hook: ProbeHook,
    pending_probe_tasks: VecDeque<ProbeTask>,
    selected_entry: usize,
    entries_initialized: bool,
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
        Self::with_hooks(default_probe_hook(), default_builtin_source_provider())
    }

    /// Creates an empty pane using `probe_hook` for on-demand server probes.
    pub fn with_probe_hook(probe_hook: ProbeHook) -> Self {
        Self::with_hooks(probe_hook, default_builtin_source_provider())
    }

    /// Creates an empty pane with injectable probe and built-in source hooks.
    pub fn with_hooks(
        probe_hook: ProbeHook,
        builtin_source_provider: BuiltinSourceProvider,
    ) -> Self {
        Self {
            entries: Vec::new(),
            builtin_overrides: BuiltinMcpOverridesConfig::default(),
            builtin_source_provider,
            builtin_confirm: BuiltinConfirmPane::default(),
            probe_states: HashMap::new(),
            expanded_probe_entries: HashSet::new(),
            probe_hook,
            pending_probe_tasks: VecDeque::new(),
            selected_entry: 0,
            entries_initialized: false,
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
        let first_load = !self.entries_initialized;
        let entries_were_empty = self.entries.is_empty();
        let selected_name = self.selected_user_entry().map(|entry| entry.name.clone());
        self.probe_states.retain(|name, _| {
            builtin_server_by_name(name).is_some()
                || entries.iter().any(|entry| entry.name == *name)
        });
        self.expanded_probe_entries.retain(|name| {
            builtin_server_by_name(name).is_some()
                || entries.iter().any(|entry| entry.name == *name)
        });
        for entry in &entries {
            self.probe_states.entry(entry.name.clone()).or_default();
        }
        self.entries = entries;
        self.selected_entry = selected_name
            .and_then(|name| self.entries.iter().position(|entry| entry.name == name))
            .map(|index| BUILTIN_SERVERS.len() + index)
            .unwrap_or_else(|| {
                if (first_load || entries_were_empty) && !self.entries.is_empty() {
                    BUILTIN_SERVERS.len()
                } else {
                    self.selected_entry.min(self.row_count().saturating_sub(1))
                }
            });
        self.entries_initialized = true;
    }

    /// Loads the confirmed MCP config snapshot.
    pub fn set_mcp_config(&mut self, config: &McpServersConfig) {
        self.builtin_overrides = config.builtin_overrides.clone();
        self.set_entries(config.entries.clone());
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let regions = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let lines = self
            .content_rows()
            .into_iter()
            .map(|row| {
                let dimmed = self.is_disabled_builtin_row(&row);
                let line = Line::from(row);
                if dimmed {
                    line.style(Style::default().add_modifier(Modifier::DIM))
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("MCP Servers")),
            regions[0],
        );
        frame.render_widget(Paragraph::new(NEXT_SESSION_NOTICE), regions[1]);
        self.builtin_confirm.render(frame, regions[0]);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.builtin_confirm.is_open() {
            return self.builtin_confirm.handle_key(key);
        }
        if self.edit_buffer.is_some() {
            return self.handle_editing_key(key);
        }
        match self.form {
            Form::None => self.handle_list_key(key),
            Form::Stdio | Form::Http | Form::JuteDebug => self.handle_form_key(key),
        }
    }

    pub fn form_active(&self) -> bool {
        self.form != Form::None
    }

    /// Returns whether an editor or confirmation pane must receive all keys.
    pub fn modal_active(&self) -> bool {
        self.form_active() || self.builtin_confirm.is_open()
    }

    /// Transitions `server_name` to probing and starts its injected runner.
    ///
    /// The returned handle is the pane-local completion seam: await it, then
    /// pass its `(server_name, report)` output to [`Self::apply_probe_result`].
    /// Returns `None` when the entry is missing or already has a probe in flight.
    ///
    /// # Panics
    ///
    /// Panics when called without an active Tokio runtime.
    pub fn handle_probe_started(&mut self, server_name: &str) -> Option<ProbeTask> {
        let entry = self.probe_entry(server_name)?;
        if matches!(
            self.probe_states.get(server_name),
            Some(ProbeState::Probing)
        ) {
            return None;
        }

        self.probe_states
            .insert(server_name.to_string(), ProbeState::Probing);
        self.expanded_probe_entries.remove(server_name);
        let probe = (self.probe_hook)(entry);
        let server_name = server_name.to_string();
        Some(tokio::spawn(async move {
            let report = probe.await;
            (server_name, report)
        }))
    }

    /// Takes the next probe task created by a `t` keypress, if any.
    pub fn take_probe_task(&mut self) -> Option<ProbeTask> {
        self.pending_probe_tasks.pop_front()
    }

    /// Applies a completed report only to the matching in-flight entry.
    pub fn apply_probe_result(&mut self, server_name: &str, report: ProbeReport) {
        if report.server_name != server_name {
            return;
        }
        let Some(state) = self.probe_states.get_mut(server_name) else {
            return;
        };
        if matches!(state, ProbeState::Probing) {
            *state = ProbeState::Done(report);
        }
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
            KeyCode::Char('t') => {
                self.start_selected_probe();
                None
            }
            KeyCode::Char('x') => {
                self.toggle_selected_probe_schema();
                None
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.begin_edit();
                None
            }
            KeyCode::Char(' ') => self.toggle_selected_action(),
            KeyCode::Delete => self.remove_selected_action(),
            KeyCode::Char('d') => self.toggle_or_remove_selected_action(),
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
        let Some(entry) = self.selected_user_entry().cloned() else {
            return;
        };
        self.form = match &entry.transport {
            McpServerTransport::Stdio { .. } => Form::Stdio,
            McpServerTransport::Http { .. } => Form::Http,
            McpServerTransport::JuteDebug => Form::JuteDebug,
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
        self.selected_entry = offset_index(self.selected_entry, self.row_count(), delta);
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
                    Form::Http => Form::JuteDebug,
                    Form::JuteDebug => Form::Stdio,
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
        let mut entry = self.selected_user_entry()?.clone();
        entry.enabled = !entry.enabled;
        Some(self.save_action_for(entry))
    }

    fn remove_selected_action(&self) -> Option<Action> {
        let name = self.selected_user_entry()?.name.clone();
        Some(Action::ConfigSaveRequested {
            patch: ConfigPatch::McpServerRemove { name },
        })
    }

    fn toggle_or_remove_selected_action(&mut self) -> Option<Action> {
        let Some(server) = self.selected_builtin_server() else {
            return self.remove_selected_action();
        };
        let enabled = !self.builtin_enabled(server);
        if server == BuiltinMcpServer::SpurMcp && !enabled {
            self.builtin_confirm.open();
            return None;
        }
        Some(Action::ConfigSaveRequested {
            patch: ConfigPatch::BuiltinMcpToggle { server, enabled },
        })
    }

    fn start_selected_probe(&mut self) {
        let Some(server_name) = self.selected_server_name().map(str::to_string) else {
            return;
        };
        if let Some(task) = self.handle_probe_started(&server_name) {
            self.pending_probe_tasks.push_back(task);
        }
    }

    fn toggle_selected_probe_schema(&mut self) {
        let Some(server_name) = self.selected_server_name().map(str::to_string) else {
            return;
        };
        if !matches!(
            self.probe_states.get(&server_name),
            Some(ProbeState::Done(ProbeReport {
                outcome: ProbeOutcome::ToolsListed(_),
                ..
            }))
        ) {
            return;
        }
        if !self.expanded_probe_entries.remove(&server_name) {
            self.expanded_probe_entries.insert(server_name);
        }
    }

    fn row_count(&self) -> usize {
        BUILTIN_SERVERS.len() + self.entries.len()
    }

    fn selected_builtin_server(&self) -> Option<BuiltinMcpServer> {
        BUILTIN_SERVERS.get(self.selected_entry).copied()
    }

    fn selected_user_entry(&self) -> Option<&McpServerEntry> {
        self.selected_entry
            .checked_sub(BUILTIN_SERVERS.len())
            .and_then(|index| self.entries.get(index))
    }

    fn selected_server_name(&self) -> Option<&str> {
        self.selected_builtin_server()
            .map(builtin_server_name)
            .or_else(|| self.selected_user_entry().map(|entry| entry.name.as_str()))
    }

    fn builtin_enabled(&self, server: BuiltinMcpServer) -> bool {
        match server {
            BuiltinMcpServer::SpurMcp => self.builtin_overrides.spur_mcp_enabled,
            BuiltinMcpServer::Notebook => self.builtin_overrides.notebook_enabled,
            BuiltinMcpServer::SpurWorkerMcp => self.builtin_overrides.worker_mcp_enabled,
        }
    }

    fn probe_entry(&self, server_name: &str) -> Option<McpServerEntry> {
        if let Some(server) = builtin_server_by_name(server_name) {
            return self.builtin_probe_entry(server);
        }
        self.entries
            .iter()
            .find(|entry| entry.name == server_name)
            .cloned()
    }

    fn builtin_probe_entry(&self, server: BuiltinMcpServer) -> Option<McpServerEntry> {
        let sources = (self.builtin_source_provider)();
        let transport = match server {
            BuiltinMcpServer::SpurMcp => McpServerTransport::Http {
                url: sources.spur_mcp_url?,
                headers: HashMap::new(),
            },
            BuiltinMcpServer::Notebook => {
                let notebook = sources.notebook?;
                McpServerTransport::Stdio {
                    command: notebook.command,
                    args: vec!["--mcp-proxy".into(), notebook.socket_path],
                    env: HashMap::new(),
                }
            }
            BuiltinMcpServer::SpurWorkerMcp => {
                let WorkerMcpRuntimeSource {
                    url: base_url,
                    token,
                } = sources.worker?;
                let url = format!("{base_url}?token={token}");
                let headers = HashMap::from([("Authorization".into(), format!("Bearer {token}"))]);
                McpServerTransport::Http { url, headers }
            }
        };
        Some(McpServerEntry {
            name: builtin_server_name(server).into(),
            enabled: self.builtin_enabled(server),
            transport,
        })
    }

    fn probe_status(&self, server_name: &str) -> String {
        match self.probe_states.get(server_name) {
            Some(ProbeState::Probing) => " — probing…".to_string(),
            Some(ProbeState::Done(ProbeReport {
                outcome: ProbeOutcome::ToolsListed(tools),
                ..
            })) => format!(" — {} tools", tools.len()),
            Some(ProbeState::Done(ProbeReport {
                outcome: ProbeOutcome::ConnectError(message),
                ..
            })) => format!(" — connect error: {message}"),
            Some(ProbeState::Done(ProbeReport {
                outcome: ProbeOutcome::Timeout,
                ..
            })) => " — timeout after 10s".to_string(),
            Some(ProbeState::Idle) | None => String::new(),
        }
    }

    fn append_probe_tool_rows(&self, rows: &mut Vec<String>, server_name: &str) {
        let Some(ProbeState::Done(ProbeReport {
            outcome: ProbeOutcome::ToolsListed(tools),
            ..
        })) = self.probe_states.get(server_name)
        else {
            return;
        };
        let expanded = self.expanded_probe_entries.contains(server_name);
        for tool in tools {
            let description = tool
                .description
                .as_deref()
                .map(one_line)
                .filter(|description| !description.is_empty())
                .unwrap_or_else(|| "(no description)".into());
            rows.push(format!("    {} — {description}", tool.name));
            if expanded {
                rows.push(format!("      schema: {}", tool.input_schema_json));
            }
        }
    }

    fn is_disabled_builtin_row(&self, row: &str) -> bool {
        let Some(row) = row.strip_prefix("> ").or_else(|| row.strip_prefix("  ")) else {
            return false;
        };
        BUILTIN_SERVERS.iter().copied().any(|server| {
            row.strip_prefix(builtin_server_name(server))
                .is_some_and(|suffix| suffix.starts_with(' '))
                && !self.builtin_enabled(server)
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
        let mut rows = vec![
            "a add  e edit  Space toggle custom  d toggle/remove  t probe  x schemas".into(),
            "Built-in servers".into(),
        ];
        let sources = (self.builtin_source_provider)();
        for (index, server) in BUILTIN_SERVERS.iter().copied().enumerate() {
            let marker = if index == self.selected_entry {
                ">"
            } else {
                " "
            };
            let state = if self.builtin_enabled(server) {
                "enabled"
            } else {
                "disabled"
            };
            let name = builtin_server_name(server);
            let kind = builtin_transport_kind(server);
            let source = builtin_source_label(server, &sources);
            let status = self.probe_status(name);
            rows.push(format!(
                "{marker} {name} [{kind}] {source} [{state}]{status}"
            ));
            self.append_probe_tool_rows(&mut rows, name);
        }

        rows.push("Configured servers".into());
        if self.entries.is_empty() {
            rows.push("No configured MCP servers".into());
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                let marker = if BUILTIN_SERVERS.len() + index == self.selected_entry {
                    ">"
                } else {
                    " "
                };
                let transport = match &entry.transport {
                    McpServerTransport::Stdio { .. } => "stdio",
                    McpServerTransport::Http { .. } => "http",
                    McpServerTransport::JuteDebug => "jute-debug",
                };
                let enabled = if entry.enabled { "enabled" } else { "disabled" };
                let status = self.probe_status(&entry.name);
                rows.push(format!(
                    "{marker} {} [{transport}] {enabled}{status}",
                    entry.name
                ));
                self.append_probe_tool_rows(&mut rows, &entry.name);
            }
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
            Form::JuteDebug => {}
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
        let mut rows = if self.builtin_confirm.is_open() {
            self.builtin_confirm.snapshot_rows()
        } else {
            self.content_rows()
        };
        rows.push(NEXT_SESSION_NOTICE.into());
        rows.join("\n")
    }
}

fn default_probe_hook() -> ProbeHook {
    Arc::new(|entry| {
        let future: ProbeFuture = Box::pin(async move { probe_server(&entry).await });
        future
    })
}

fn default_builtin_source_provider() -> BuiltinSourceProvider {
    Arc::new(BuiltinMcpRuntimeSources::default)
}

fn builtin_server_name(server: BuiltinMcpServer) -> &'static str {
    match server {
        BuiltinMcpServer::SpurMcp => "spur-mcp",
        BuiltinMcpServer::Notebook => "notebook",
        BuiltinMcpServer::SpurWorkerMcp => "spur-worker-mcp",
    }
}

fn builtin_server_by_name(name: &str) -> Option<BuiltinMcpServer> {
    BUILTIN_SERVERS
        .iter()
        .copied()
        .find(|server| builtin_server_name(*server) == name)
}

fn builtin_transport_kind(server: BuiltinMcpServer) -> &'static str {
    match server {
        BuiltinMcpServer::Notebook => "stdio",
        BuiltinMcpServer::SpurMcp | BuiltinMcpServer::SpurWorkerMcp => "http",
    }
}

fn builtin_source_label(server: BuiltinMcpServer, sources: &BuiltinMcpRuntimeSources) -> String {
    match server {
        BuiltinMcpServer::SpurMcp => sources.spur_mcp_url.as_deref().map_or_else(
            || "source unavailable".into(),
            |url| format!("runtime: {url}"),
        ),
        BuiltinMcpServer::Notebook => sources.notebook.as_ref().map_or_else(
            || "source unavailable".into(),
            |notebook| format!("resolved: {}", notebook.command),
        ),
        BuiltinMcpServer::SpurWorkerMcp => sources.worker.as_ref().map_or_else(
            || "not running".into(),
            |worker| format!("live: {}", worker.url),
        ),
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, style::Modifier, Terminal};
    use spur_acp::config::{
        BuiltinMcpServer, ConfigPatch, McpServerEntry, McpServerTransport, McpServersConfig,
    };
    use spur_mcp::probe::{ProbeOutcome, ProbeReport, ProbedTool};

    use crate::action::Action;

    use super::{
        BuiltinMcpRuntimeSources, BuiltinSourceProvider, McpServersPane, NotebookMcpRuntimeSource,
        ProbeFuture, ProbeHook, WorkerMcpRuntimeSource,
    };

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

    fn focus_entry(pane: &mut McpServersPane, name: &str) {
        let mut seen = HashSet::new();
        loop {
            let snapshot = pane.render_snapshot();
            let selected = snapshot
                .lines()
                .find(|line| line.starts_with("> "))
                .unwrap_or_else(|| panic!("pane has no focused row:\n{snapshot}"));
            let selected_name = selected
                .strip_prefix("> ")
                .and_then(|line| line.split_whitespace().next());
            if selected_name == Some(name) {
                return;
            }
            assert!(
                seen.insert(selected.to_string()),
                "could not focus `{name}`:\n{snapshot}"
            );
            assert!(pane.handle_key(key(KeyCode::Down)).is_none());
        }
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

    fn fake_probe_hook(
        calls: Arc<AtomicUsize>,
        outcomes: HashMap<String, ProbeOutcome>,
    ) -> ProbeHook {
        let outcomes = Arc::new(outcomes);
        Arc::new(move |entry: McpServerEntry| {
            calls.fetch_add(1, Ordering::SeqCst);
            let outcome = outcomes
                .get(&entry.name)
                .cloned()
                .unwrap_or_else(|| ProbeOutcome::ConnectError("missing fake outcome".into()));
            let future: ProbeFuture = Box::pin(async move {
                ProbeReport {
                    server_name: entry.name,
                    outcome,
                }
            });
            future
        })
    }

    fn tools_outcome() -> ProbeOutcome {
        ProbeOutcome::ToolsListed(vec![
            ProbedTool {
                name: "echo".into(),
                description: Some("Echo one line".into()),
                input_schema_json: r#"{"type":"object","required":["message"]}"#.into(),
            },
            ProbedTool {
                name: "add".into(),
                description: Some("Add\nnumbers".into()),
                input_schema_json: r#"{"type":"object","required":["left","right"]}"#.into(),
            },
        ])
    }

    fn report(server_name: &str, outcome: ProbeOutcome) -> ProbeReport {
        ProbeReport {
            server_name: server_name.into(),
            outcome,
        }
    }

    #[test]
    fn builtin_block_renders_transport_sources_and_state_badges() {
        let mut config = McpServersConfig::default();
        config.builtin_overrides.notebook_enabled = false;
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&config);

        let text = pane.render_snapshot();

        assert!(text.contains("Built-in servers"), "{text}");
        assert!(
            text.lines().any(|line| {
                line.contains("spur-mcp")
                    && line.contains("[http]")
                    && line.contains("source unavailable")
                    && line.contains("[enabled]")
            }),
            "{text}"
        );
        assert!(
            text.lines().any(|line| {
                line.contains("notebook")
                    && line.contains("[stdio]")
                    && line.contains("source unavailable")
                    && line.contains("[disabled]")
            }),
            "{text}"
        );
        assert!(
            text.lines().any(|line| {
                line.contains("spur-worker-mcp")
                    && line.contains("[http]")
                    && line.contains("not running")
                    && line.contains("[enabled]")
            }),
            "{text}"
        );
        assert!(text.contains("applies to next session"), "{text}");
    }

    #[test]
    fn disabled_builtin_row_renders_dimmed() {
        let mut config = McpServersConfig::default();
        config.builtin_overrides.notebook_enabled = false;
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&config);
        let snapshot = pane.render_snapshot();
        let width = snapshot
            .lines()
            .map(str::chars)
            .map(Iterator::count)
            .max()
            .unwrap_or_default()
            .saturating_add(2)
            .try_into()
            .expect("snapshot width should fit u16");
        let height = snapshot
            .lines()
            .count()
            .saturating_add(2)
            .try_into()
            .expect("snapshot height should fit u16");
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| pane.render(frame, frame.area()))
            .expect("pane should render");
        let buffer = terminal.backend().buffer();
        let notebook_start = buffer
            .content
            .windows("notebook".len())
            .position(|cells| {
                cells.iter().map(|cell| cell.symbol()).collect::<String>() == "notebook"
            })
            .expect("notebook row should render");
        let notebook_cell = &buffer.content[notebook_start];

        assert!(
            notebook_cell.modifier.contains(Modifier::DIM),
            "notebook row should be dimmed"
        );
    }

    #[test]
    fn spur_mcp_reenable_emits_builtin_toggle_without_confirmation() {
        let mut config = McpServersConfig::default();
        config.builtin_overrides.spur_mcp_enabled = false;
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&config);
        focus_entry(&mut pane, "spur-mcp");

        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('d'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::BuiltinMcpToggle {
                    server: BuiltinMcpServer::SpurMcp,
                    enabled: true,
                }
            })
        ));
    }

    #[test]
    fn notebook_disable_emits_builtin_toggle_without_confirmation() {
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig::default());
        focus_entry(&mut pane, "notebook");

        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('d'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::BuiltinMcpToggle {
                    server: BuiltinMcpServer::Notebook,
                    enabled: false,
                }
            })
        ));
        assert!(!pane.render_snapshot().contains("Disable spur-mcp"));
    }

    #[test]
    fn worker_disable_emits_builtin_toggle_without_confirmation() {
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig::default());
        focus_entry(&mut pane, "spur-worker-mcp");

        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('d'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::BuiltinMcpToggle {
                    server: BuiltinMcpServer::SpurWorkerMcp,
                    enabled: false,
                }
            })
        ));
        assert!(!pane.render_snapshot().contains("Disable spur-mcp"));
    }

    #[test]
    fn spur_mcp_disable_requires_confirmation_before_emitting_patch() {
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig::default());
        focus_entry(&mut pane, "spur-mcp");

        assert!(pane.handle_key(key(KeyCode::Char('d'))).is_none());
        let dialog = pane.render_snapshot();
        assert!(dialog.contains("Disable spur-mcp"), "{dialog}");
        assert!(dialog.contains("delegation/plan/solve tools"), "{dialog}");
        assert!(dialog.contains("applies to next session"), "{dialog}");
        assert!(matches!(
            pane.handle_key(key(KeyCode::Enter)),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::BuiltinMcpToggle {
                    server: BuiltinMcpServer::SpurMcp,
                    enabled: false,
                }
            })
        ));
    }

    #[test]
    fn spur_mcp_disable_confirmation_cancel_emits_no_patch() {
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig::default());
        focus_entry(&mut pane, "spur-mcp");

        assert!(pane.handle_key(key(KeyCode::Char('d'))).is_none());
        assert!(pane.render_snapshot().contains("Disable spur-mcp"));
        assert!(pane.handle_key(key(KeyCode::Esc)).is_none());
        assert!(!pane.render_snapshot().contains("Disable spur-mcp"));
        assert!(pane.handle_key(key(KeyCode::Enter)).is_none());
    }

    #[tokio::test]
    async fn probe_key_renders_probing_until_completion_is_applied() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = fake_probe_hook(
            Arc::clone(&calls),
            HashMap::from([("a".into(), tools_outcome())]),
        );
        let mut pane = McpServersPane::with_probe_hook(hook);
        pane.set_entries(vec![test_http_entry("a")]);
        focus_entry(&mut pane, "a");

        assert!(pane.handle_key(key(KeyCode::Char('t'))).is_none());
        assert!(pane.render_snapshot().contains("probing…"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let (server_name, result) = pane
            .take_probe_task()
            .expect("probe task should be queued")
            .await
            .expect("fake probe task should join");
        assert!(pane.render_snapshot().contains("probing…"));

        pane.apply_probe_result(&server_name, result);
        assert!(!pane.render_snapshot().contains("probing…"));
    }

    #[tokio::test]
    async fn builtin_probe_uses_transient_entries_from_runtime_provider() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_hook = Arc::clone(&captured);
        let hook: ProbeHook = Arc::new(move |entry: McpServerEntry| {
            captured_for_hook
                .lock()
                .expect("capture lock should be available")
                .push(entry.clone());
            let future: ProbeFuture = Box::pin(async move {
                ProbeReport {
                    server_name: entry.name,
                    outcome: ProbeOutcome::ToolsListed(Vec::new()),
                }
            });
            future
        });
        let sources: BuiltinSourceProvider = Arc::new(|| BuiltinMcpRuntimeSources {
            spur_mcp_url: Some("http://127.0.0.1:7000/mcp".into()),
            notebook: Some(NotebookMcpRuntimeSource {
                command: "/opt/spur-notebook".into(),
                socket_path: "/tmp/spur-notebook-test.sock".into(),
            }),
            worker: Some(WorkerMcpRuntimeSource {
                url: "http://127.0.0.1:7777/mcp".into(),
                token: "worker-secret".into(),
            }),
        });
        let mut pane = McpServersPane::with_hooks(hook, sources);
        pane.set_mcp_config(&McpServersConfig::default());

        for name in ["spur-mcp", "notebook", "spur-worker-mcp"] {
            focus_entry(&mut pane, name);
            assert!(pane.handle_key(key(KeyCode::Char('t'))).is_none());
            pane.take_probe_task()
                .expect("built-in probe task should be queued")
                .await
                .expect("built-in probe task should join");
        }

        let entries = captured.lock().expect("capture lock should be available");
        assert!(matches!(
            &entries[0],
            McpServerEntry {
                name,
                transport: McpServerTransport::Http { url, headers },
                ..
            } if name == "spur-mcp"
                && url == "http://127.0.0.1:7000/mcp"
                && headers.is_empty()
        ));
        assert!(matches!(
            &entries[1],
            McpServerEntry {
                name,
                transport: McpServerTransport::Stdio { command, args, env },
                ..
            } if name == "notebook"
                && command == "/opt/spur-notebook"
                && args == &["--mcp-proxy", "/tmp/spur-notebook-test.sock"]
                && env.is_empty()
        ));
        assert!(matches!(
            &entries[2],
            McpServerEntry {
                name,
                transport: McpServerTransport::Http { url, headers },
                ..
            } if name == "spur-worker-mcp"
                && url == "http://127.0.0.1:7777/mcp?token=worker-secret"
                && headers.get("Authorization").map(String::as_str)
                    == Some("Bearer worker-secret")
        ));
    }

    #[tokio::test]
    async fn successful_probe_renders_tools_and_toggles_schemas() {
        let hook = fake_probe_hook(
            Arc::new(AtomicUsize::new(0)),
            HashMap::from([("a".into(), tools_outcome())]),
        );
        let mut pane = McpServersPane::with_probe_hook(hook);
        pane.set_entries(vec![test_http_entry("a")]);
        focus_entry(&mut pane, "a");
        pane.handle_key(key(KeyCode::Char('t')));
        pane.apply_probe_result("a", report("a", tools_outcome()));

        let collapsed = pane.render_snapshot();
        assert!(collapsed.contains("echo") && collapsed.contains("Echo one line"));
        assert!(collapsed.contains("add") && collapsed.contains("Add numbers"));
        assert!(!collapsed.contains(r#""required""#), "{collapsed}");

        pane.handle_key(key(KeyCode::Char('x')));
        let expanded = pane.render_snapshot();
        assert!(expanded.contains(r#"{"type":"object","required":["message"]}"#));
        assert!(expanded.contains(r#"{"type":"object","required":["left","right"]}"#));

        pane.handle_key(key(KeyCode::Char('x')));
        assert!(!pane.render_snapshot().contains(r#""required""#));
    }

    #[tokio::test]
    async fn probe_errors_and_timeouts_render_inline() {
        let hook = fake_probe_hook(
            Arc::new(AtomicUsize::new(0)),
            HashMap::from([
                ("a".into(), ProbeOutcome::ConnectError("refused".into())),
                ("b".into(), ProbeOutcome::Timeout),
            ]),
        );
        let mut pane = McpServersPane::with_probe_hook(hook);
        pane.set_entries(vec![test_http_entry("a"), test_http_entry("b")]);
        focus_entry(&mut pane, "a");

        pane.handle_key(key(KeyCode::Char('t')));
        pane.apply_probe_result(
            "a",
            report("a", ProbeOutcome::ConnectError("refused".into())),
        );
        focus_entry(&mut pane, "b");
        pane.handle_key(key(KeyCode::Char('t')));
        pane.apply_probe_result("b", report("b", ProbeOutcome::Timeout));

        let text = pane.render_snapshot();
        assert!(text.contains("connect error: refused"), "{text}");
        assert!(text.contains("timeout after 10s"), "{text}");
    }

    #[tokio::test]
    async fn probe_key_is_noop_while_entry_is_probing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = fake_probe_hook(
            Arc::clone(&calls),
            HashMap::from([("a".into(), tools_outcome())]),
        );
        let mut pane = McpServersPane::with_probe_hook(hook);
        pane.set_entries(vec![test_http_entry("a")]);
        focus_entry(&mut pane, "a");

        pane.handle_key(key(KeyCode::Char('t')));
        pane.handle_key(key(KeyCode::Char('t')));

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(pane.take_probe_task().is_some());
        assert!(pane.take_probe_task().is_none());
    }

    #[tokio::test]
    async fn probe_result_is_isolated_to_starting_entry() {
        let hook = fake_probe_hook(
            Arc::new(AtomicUsize::new(0)),
            HashMap::from([
                ("a".into(), tools_outcome()),
                ("b".into(), ProbeOutcome::ConnectError("b failed".into())),
            ]),
        );
        let mut pane = McpServersPane::with_probe_hook(hook);
        pane.set_entries(vec![test_http_entry("a"), test_http_entry("b")]);
        focus_entry(&mut pane, "a");

        pane.handle_key(key(KeyCode::Char('t')));
        focus_entry(&mut pane, "b");
        pane.handle_key(key(KeyCode::Char('t')));
        pane.apply_probe_result("a", report("a", tools_outcome()));

        let text = pane.render_snapshot();
        assert!(text.contains("echo"), "{text}");
        assert!(text
            .lines()
            .any(|line| line.contains("b [http]") && line.contains("probing…")));

        pane.apply_probe_result(
            "b",
            report("a", ProbeOutcome::ConnectError("wrong entry".into())),
        );
        let text = pane.render_snapshot();
        assert!(!text.contains("wrong entry"), "{text}");
        assert!(text
            .lines()
            .any(|line| line.contains("b [http]") && line.contains("probing…")));
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
        focus_entry(&mut pane, "ghost");

        let text = pane.render_snapshot();

        assert!(text.contains("ghost") && text.contains("http"), "{text}");
        assert!(text.contains("disabled"), "{text}");
        assert!(text.contains("applies to next session"), "{text}");
    }

    #[test]
    fn list_toggle_and_remove_emit_existing_config_patches() {
        let mut pane = McpServersPane::new();
        pane.set_entries(vec![disabled_entry("ghost")]);
        focus_entry(&mut pane, "ghost");

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
    fn jute_debug_add_form_saves_managed_transport() {
        let mut pane = McpServersPane::new();
        pane.handle_key(key(KeyCode::Char('a')));
        enter_text(&mut pane, "jute-debug");
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Enter));
        pane.handle_key(key(KeyCode::Enter));

        let text = pane.render_snapshot();
        assert!(text.contains("transport: jute-debug"), "{text}");
        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('s'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        transport: McpServerTransport::JuteDebug,
                        ..
                    }
                }
            }) if name == "jute-debug"
        ));
    }

    #[test]
    fn edit_loads_selected_entry_and_saves_an_upsert() {
        let entry = test_http_entry("github");
        let mut pane = McpServersPane::new();
        pane.set_mcp_config(&McpServersConfig {
            builtin_overrides: Default::default(),
            entries: vec![entry.clone()],
        });
        focus_entry(&mut pane, "github");

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
