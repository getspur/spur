use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

use agent_client_protocol::schema::{
    InitializeRequest, ListSessionsRequest, ProtocolVersion, SessionInfo,
};
use spur_acp::connection::{
    AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter,
};
use spur_acp::types::{AgentHealth, TransportKind};

use crate::orchestrator::session_discovery::discovery_for_kind;
use crate::orchestrator::{Orchestrator, MAX_SESSION_LIST_PAGES, MAX_SESSION_LIST_SESSIONS};

fn merge_sessions_by_id(
    scoped_sessions: Vec<SessionInfo>,
    broad_sessions: Vec<SessionInfo>,
) -> Vec<SessionInfo> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for session in scoped_sessions.into_iter().chain(broad_sessions) {
        if seen.insert(session.session_id.clone()) {
            merged.push(session);
            if merged.len() >= MAX_SESSION_LIST_SESSIONS {
                break;
            }
        }
    }

    merged
}

impl Orchestrator {
    /// Initialize: scan $PATH for agents declared in the embedded seed
    /// template (`spur_acp::config::load_seed_template`), register those
    /// whose `command` is on $PATH.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        let seeds = spur_acp::config::load_seed_template();
        let mut found = Vec::new();
        for seed in seeds.entries {
            let ok = tokio::process::Command::new("which")
                .arg(&seed.command)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                info!(agent = %seed.name, command = %seed.command, "Found agent");
                found.push(seed.name.clone());
                self.registry.register(seed);
            }
        }
        Ok(found)
    }

    /// Health-check all registered agents.
    pub async fn check_agents(&mut self) -> Vec<(String, AgentHealth)> {
        let agents: Vec<_> = self.registry.list().into_iter().cloned().collect();
        let mut results = Vec::new();

        for config in &agents {
            let mut connection = self.create_connection(config, None);
            let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
            let health = match connection.initialize(init_request).await {
                Ok(_) => {
                    let _ = connection.shutdown().await;
                    AgentHealth::Ready
                }
                Err(e) => AgentHealth::Error(e.to_string()),
            };
            results.push((config.name.clone(), health));
        }

        // Update health after iteration to avoid borrow conflict.
        for (name, health) in &results {
            self.registry.set_health(name, health.clone());
        }

        results
    }

    /// Resolve and initialize a brain agent connection without starting a full session.
    ///
    /// Steps: resolve brain name from config → get brain_config from registry →
    /// create connection → initialize. Returns (connection, brain_name).
    pub(super) fn selected_brain_name(&self, brain_override: Option<&str>) -> String {
        brain_override
            .unwrap_or(&self.config.brain.default)
            .to_string()
    }

    pub(super) async fn connect_brain(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Result<(
        Box<dyn spur_acp::AgentConnection>,
        String,
        agent_client_protocol::schema::InitializeResponse,
    )> {
        let brain_name = self.selected_brain_name(brain_override);

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        let mut connection = self.create_connection(&brain_config, permission_tx);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        let init_response = connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(brain = %brain_name, "Brain agent connected and initialized");
        Ok((connection, brain_name, init_response))
    }

    pub(super) fn create_connection(
        &self,
        config: &spur_acp::config::AgentConfig,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Box<dyn AgentConnection> {
        // L1a: effective_args folds skip_permissions_args into the spawn
        // args when bypass is on.
        let args = config.effective_args();
        let perms = config.effective_permissions();
        // L2: when bypass is on, short-circuit permission requests by
        // passing None, which activates spur-acp's auto_approve fast-path.
        // Only meaningful for transports that surface ACP permission
        // callbacks (ACP native); other transports ignore the value.
        let perm_tx = if perms.skip { None } else { permission_tx };

        build_connection_from_transport(config, args, perm_tx, &self.repo_root)
    }

    pub(super) async fn list_sessions_from_rpc(
        conn: &mut dyn AgentConnection,
        repo_root: &Path,
    ) -> Result<Vec<SessionInfo>> {
        // Some agents treat ACP `cwd` as a prefix and some as an exact
        // match. Query the repo root first so repo scoping is explicit, then
        // query broadly so subdirectory/worktree sessions are still available
        // for local prefix classification.
        let scoped_sessions =
            Self::list_sessions_from_rpc_with_cwd(conn, Some(repo_root.to_path_buf())).await?;

        // If scoped results already fill the budget, skip the expensive broad
        // query — merge would discard the broad results anyway.
        if scoped_sessions.len() >= MAX_SESSION_LIST_SESSIONS {
            return Ok(scoped_sessions);
        }

        let broad_sessions = Self::list_sessions_from_rpc_with_cwd(conn, None).await?;

        Ok(merge_sessions_by_id(scoped_sessions, broad_sessions))
    }

    async fn list_sessions_from_rpc_with_cwd(
        conn: &mut dyn AgentConnection,
        cwd: Option<PathBuf>,
    ) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();
        let mut cursor: Option<String> = None;

        for page_index in 0..MAX_SESSION_LIST_PAGES {
            let list_req = ListSessionsRequest::new()
                .cwd(cwd.clone())
                .cursor(cursor.clone());
            let response = conn.list_sessions(list_req).await?;
            let next_cursor = response.next_cursor;

            let remaining = MAX_SESSION_LIST_SESSIONS.saturating_sub(sessions.len());
            if response.sessions.len() > remaining {
                sessions.extend(response.sessions.into_iter().take(remaining));
                warn!(
                    count = sessions.len(),
                    "list_sessions session cap reached; truncating remaining pages"
                );
                break;
            }
            sessions.extend(response.sessions);

            if next_cursor.is_none() {
                break;
            }
            if page_index + 1 >= MAX_SESSION_LIST_PAGES {
                warn!(
                    pages = MAX_SESSION_LIST_PAGES,
                    "list_sessions page cap reached; truncating remaining pages"
                );
                break;
            }
            if next_cursor == cursor {
                warn!("list_sessions cursor did not advance; breaking to avoid loop");
                break;
            }
            cursor = next_cursor;
        }

        Ok(sessions)
    }

    /// Fallback: read sessions from an agent's local storage on disk.
    pub(super) fn list_sessions_from_disk(
        agent_config: &spur_acp::config::AgentConfig,
    ) -> Result<Vec<SessionInfo>> {
        let kind = agent_config.kind;
        let discovery = discovery_for_kind(kind)
            .or_else(|| {
                // Graceful fallback: if kind is Generic (missing from config),
                // try to infer from the agent name so users with stale configs
                // still get disk fallback for known agents.
                if kind != spur_acp::AgentKind::Generic {
                    return None;
                }
                let inferred = spur_acp::AgentKind::from_name(&agent_config.name);
                if inferred != kind {
                    tracing::info!(
                        agent = %agent_config.name,
                        ?kind,
                        ?inferred,
                        "inferred agent kind from name for disk fallback"
                    );
                }
                discovery_for_kind(inferred)
            })
            .ok_or_else(|| {
                anyhow!(
                    "No filesystem fallback available for agent '{}' (kind: {:?}). \
                     Add `kind = \"{}\"` to its .spur/config.toml entry.",
                    agent_config.name,
                    kind,
                    agent_config.name
                )
            })?;
        discovery.discover()
    }

    /// Read conversation history from an agent session's JSONL file on disk.
    /// Returns (role, text) pairs for user and assistant entries.
    pub(super) fn read_session_history_from_disk(
        session_uuid: &str,
    ) -> Vec<spur_acp::HistoryEntry> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let jsonl_path = home.join(format!(".kiro/sessions/cli/{}.jsonl", session_uuid));

        if jsonl_path.exists() {
            return std::fs::read_to_string(&jsonl_path)
                .map(|content| parse_kiro_history_from_jsonl(&content))
                .unwrap_or_default();
        }

        if let Some(context_path) = find_kimi_context_path(&home, session_uuid) {
            return std::fs::read_to_string(&context_path)
                .map(|content| parse_kimi_history_from_jsonl(&content))
                .unwrap_or_default();
        }

        if let Some(path) = find_claude_session_path(&home, session_uuid) {
            return std::fs::read_to_string(&path)
                .map(|content| parse_claude_history_from_jsonl(&content))
                .unwrap_or_default();
        }

        if let Some(path) = find_codex_session_path(&home, session_uuid) {
            return std::fs::read_to_string(&path)
                .map(|content| parse_codex_history_from_jsonl(&content))
                .unwrap_or_default();
        }

        Vec::new()
    }
}

fn parse_kiro_history_from_jsonl(content: &str) -> Vec<spur_acp::HistoryEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("");

        // Concatenate ALL text content blocks (messages can have multiple).
        let text = json
            .pointer("/data/content")
            .and_then(|arr| arr.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let item_kind = item.get("kind").and_then(|v| v.as_str())?;
                        if item_kind == "text" {
                            item.get("data").and_then(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        if text.is_empty() {
            continue;
        }

        match kind {
            "Prompt" => entries.push(spur_acp::HistoryEntry {
                role: "user".into(),
                text,
            }),
            "AssistantMessage" => entries.push(spur_acp::HistoryEntry {
                role: "assistant".into(),
                text,
            }),
            _ => {} // Skip ToolResults, etc. for v1
        }
    }
    entries
}

fn find_kimi_context_path(home: &Path, session_uuid: &str) -> Option<PathBuf> {
    let sessions_root = home.join(".kimi/sessions");
    let mut hash_dirs = std::fs::read_dir(&sessions_root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    hash_dirs.sort();

    for hash_dir in hash_dirs {
        let context_path = hash_dir.join(session_uuid).join("context.jsonl");
        if context_path.is_file() {
            return Some(context_path);
        }
    }

    None
}

fn parse_kimi_history_from_jsonl(content: &str) -> Vec<spur_acp::HistoryEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match json.get("role").and_then(|v| v.as_str()).unwrap_or("") {
            "user" => {
                let text = json
                    .get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if !text.is_empty() {
                    entries.push(spur_acp::HistoryEntry {
                        role: "user".into(),
                        text: text.to_string(),
                    });
                }
            }
            "assistant" => {
                let text = json
                    .get("content")
                    .and_then(|value| value.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| {
                                if block.get("type").and_then(|value| value.as_str())
                                    == Some("text")
                                {
                                    block.get("text").and_then(|value| value.as_str())
                                } else {
                                    None
                                }
                            })
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    entries.push(spur_acp::HistoryEntry {
                        role: "assistant".into(),
                        text,
                    });
                }
            }
            _ => {}
        }
    }
    entries
}

fn find_claude_session_path(home: &Path, session_uuid: &str) -> Option<PathBuf> {
    let projects_root = home.join(".claude/projects");
    let mut project_dirs = std::fs::read_dir(&projects_root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    project_dirs.sort();

    for project_dir in project_dirs {
        let session_path = project_dir.join(format!("{session_uuid}.jsonl"));
        if session_path.is_file() {
            return Some(session_path);
        }
    }

    None
}

fn parse_claude_history_from_jsonl(content: &str) -> Vec<spur_acp::HistoryEntry> {
    let mut entries = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match json.get("type").and_then(|value| value.as_str()) {
            Some("user") => {
                if json
                    .get("isMeta")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }

                let Some(text) = json
                    .pointer("/message/content")
                    .and_then(|value| value.as_str())
                    .map(|text| text.trim_end())
                else {
                    continue;
                };

                if !text.trim().is_empty() {
                    entries.push(spur_acp::HistoryEntry {
                        role: "user".into(),
                        text: text.to_string(),
                    });
                }
            }
            Some("assistant") => {
                let text = match json.pointer("/message/content") {
                    Some(value) => {
                        if let Some(text) = value.as_str() {
                            text.trim_end().to_string()
                        } else if let Some(blocks) = value.as_array() {
                            blocks
                                .iter()
                                .filter_map(|block| {
                                    if block.get("type").and_then(|value| value.as_str())
                                        == Some("text")
                                    {
                                        block.get("text").and_then(|value| value.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                                .trim_end()
                                .to_string()
                        } else {
                            String::new()
                        }
                    }
                    None => String::new(),
                };

                if !text.trim().is_empty() {
                    entries.push(spur_acp::HistoryEntry {
                        role: "assistant".into(),
                        text,
                    });
                }
            }
            _ => {}
        }
    }
    entries
}

fn find_codex_session_path(home: &Path, session_uuid: &str) -> Option<PathBuf> {
    let sessions_root = home.join(".codex/sessions");
    let mut year_dirs = std::fs::read_dir(&sessions_root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    year_dirs.sort();

    for year_dir in year_dirs {
        let Ok(months) = std::fs::read_dir(&year_dir) else {
            continue;
        };
        let mut month_dirs = months
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        month_dirs.sort();

        for month_dir in month_dirs {
            let Ok(days) = std::fs::read_dir(&month_dir) else {
                continue;
            };
            let mut day_dirs = days
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>();
            day_dirs.sort();

            for day_dir in day_dirs {
                let Ok(files) = std::fs::read_dir(&day_dir) else {
                    continue;
                };
                let mut files = files
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path.is_file())
                    .collect::<Vec<_>>();
                files.sort();

                for path in files {
                    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if file_name.contains(session_uuid) && file_name.ends_with(".jsonl") {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

fn parse_codex_history_from_jsonl(content: &str) -> Vec<spur_acp::HistoryEntry> {
    let mut entries = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let is_message = json.get("type").and_then(|value| value.as_str()) == Some("message")
            || json.get("kind").and_then(|value| value.as_str()) == Some("message");
        if !is_message {
            continue;
        }

        let Some(role @ ("user" | "assistant")) = json.get("role").and_then(|value| value.as_str())
        else {
            continue;
        };

        let text = match json.get("content") {
            Some(value) => {
                if let Some(text) = value.as_str() {
                    text.trim_end().to_string()
                } else if let Some(blocks) = value.as_array() {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                                block.get("text").and_then(|value| value.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim_end()
                        .to_string()
                } else {
                    String::new()
                }
            }
            None => String::new(),
        };

        if !text.trim().is_empty() {
            entries.push(spur_acp::HistoryEntry {
                role: role.into(),
                text,
            });
        }
    }
    entries
}

/// Build a boxed `AgentConnection` from the transport declared in `config`.
///
/// Single source of truth for the `match transport { Acp/Stdio/CliWrap/StreamJson }`
/// arms. Both `Orchestrator::create_connection` (brain + resume paths) and
/// `run_one_worker_attempt` (worker spawn) call this — previously each had
/// its own copy of the match, and would drift when transports changed.
///
/// `spawn_args` is the final, bypass-aware spawn argv (callers invoke
/// `config.effective_args()` before passing them in). `permission_tx` is
/// honored only by the ACP transport; other transports ignore it.
pub(super) fn build_connection_from_transport(
    config: &spur_acp::config::AgentConfig,
    spawn_args: Vec<String>,
    permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    repo_root: &Path,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => {
            let mut conn = NativeAcpConnection::new_with_kind(
                config.name.clone(),
                config.command.clone(),
                spawn_args,
                config.kind,
                permission_tx,
            );
            conn.set_repo_root(repo_root.to_path_buf());
            Box::new(conn)
        }
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate the global `HOME` environment variable.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap();
        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let result = f();
        if let Some(home) = orig_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        result
    }

    fn write_kimi_context(home: &Path, session_uuid: &str, content: &str) {
        let session_dir = home.join(".kimi/sessions/fake-cwd-hash").join(session_uuid);
        std::fs::create_dir_all(&session_dir).expect("create kimi session dir");
        std::fs::write(session_dir.join("context.jsonl"), content).expect("write context");
    }

    fn write_claude_session(home: &Path, project: &str, session_uuid: &str, content: &str) {
        let session_dir = home.join(".claude/projects").join(project);
        std::fs::create_dir_all(&session_dir).expect("create claude session dir");
        std::fs::write(session_dir.join(format!("{session_uuid}.jsonl")), content)
            .expect("write claude session");
    }

    fn write_codex_session(home: &Path, session_uuid: &str, content: &str) -> PathBuf {
        let session_dir = home.join(".codex/sessions/2026/05/26");
        std::fs::create_dir_all(&session_dir).expect("create codex session dir");
        let path = session_dir.join(format!("rollout-2026-05-26T00-00-00-{session_uuid}.jsonl"));
        std::fs::write(&path, content).expect("write codex session");
        path
    }

    fn history_entries(session_uuid: &str) -> Vec<(String, String)> {
        Orchestrator::read_session_history_from_disk(session_uuid)
            .into_iter()
            .map(|entry| (entry.role, entry.text))
            .collect()
    }

    fn entry_pairs(entries: Vec<spur_acp::HistoryEntry>) -> Vec<(String, String)> {
        entries
            .into_iter()
            .map(|entry| (entry.role, entry.text))
            .collect()
    }

    #[test]
    fn reads_kimi_user_message_from_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_kimi_context(
            temp.path(),
            "session-user",
            r#"{"role":"user","content":"Working directory: /repo/spur\n\nTask: fix it"}"#,
        );

        let entries = with_home(temp.path(), || history_entries("session-user"));

        assert_eq!(
            entries,
            vec![(
                "user".to_string(),
                "Working directory: /repo/spur\n\nTask: fix it".to_string()
            )]
        );
    }

    #[test]
    fn reads_kimi_assistant_text_and_skips_think_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_kimi_context(
            temp.path(),
            "session-assistant",
            r#"{"role":"assistant","content":[{"type":"think","think":"hidden reasoning","encrypted":null},{"type":"text","text":"visible response"}],"tool_calls":[]}"#,
        );

        let entries = with_home(temp.path(), || history_entries("session-assistant"));

        assert_eq!(
            entries,
            vec![("assistant".to_string(), "visible response".to_string())]
        );
    }

    #[test]
    fn reads_kimi_assistant_multiple_text_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_kimi_context(
            temp.path(),
            "session-multi-text",
            r#"{"role":"assistant","content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}"#,
        );

        let entries = with_home(temp.path(), || history_entries("session-multi-text"));

        assert_eq!(
            entries,
            vec![("assistant".to_string(), "first\nsecond".to_string())]
        );
    }

    #[test]
    fn reads_kimi_mixed_file_and_skips_tools_and_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_kimi_context(
            temp.path(),
            "session-mixed",
            r#"{"role":"_system_prompt","content":"system"}
{"role":"user","content":"hello"}
{"role":"tool","content":"tool result","tool_call_id":"call-1"}
{"role":"assistant","content":[{"type":"think","think":"hidden"},{"type":"text","text":"hi there"}]}
{"role":"_usage","content":"usage"}
{"role":"_checkpoint","content":"checkpoint"}"#,
        );

        let entries = with_home(temp.path(), || history_entries("session-mixed"));

        assert_eq!(
            entries,
            vec![
                ("user".to_string(), "hello".to_string()),
                ("assistant".to_string(), "hi there".to_string())
            ]
        );
    }

    #[test]
    fn missing_kimi_session_returns_empty_history() {
        let temp = tempfile::tempdir().expect("tempdir");

        let entries = with_home(temp.path(), || history_entries("missing-session"));

        assert!(entries.is_empty());
    }

    #[test]
    fn parses_claude_user_string_content_as_history() {
        let entries = parse_claude_history_from_jsonl(
            r#"{"type":"user","message":{"content":"hello from claude"}}"#,
        );

        assert_eq!(
            entry_pairs(entries),
            vec![("user".to_string(), "hello from claude".to_string())]
        );
    }

    #[test]
    fn skips_claude_user_meta_entries() {
        let entries = parse_claude_history_from_jsonl(
            r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}
{"type":"user","isMeta":true,"message":{"content":"/model opus"}}"#,
        );

        assert!(entries.is_empty());
    }

    #[test]
    fn skips_claude_user_tool_result_arrays() {
        let entries = parse_claude_history_from_jsonl(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ignored"}]}}"#,
        );

        assert!(entries.is_empty());
    }

    #[test]
    fn parses_claude_assistant_text_blocks() {
        let entries = parse_claude_history_from_jsonl(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"},{"type":"tool_use","name":"Edit"},{"type":"text","text":"second"}]}}"#,
        );

        assert_eq!(
            entry_pairs(entries),
            vec![("assistant".to_string(), "first\nsecond".to_string())]
        );
    }

    #[test]
    fn skips_blank_and_malformed_claude_lines() {
        let entries = parse_claude_history_from_jsonl(
            r#"
not-json
{"type":"user","message":{"content":"first"}}

{"type":"assistant","message":{"content":"second"}}"#,
        );

        assert_eq!(
            entry_pairs(entries),
            vec![
                ("user".to_string(), "first".to_string()),
                ("assistant".to_string(), "second".to_string())
            ]
        );
    }

    #[test]
    fn claude_preserves_order_user_assistant_user() {
        let entries = parse_claude_history_from_jsonl(
            r#"{"type":"user","message":{"content":"one"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}
{"type":"user","message":{"content":"three"}}"#,
        );

        assert_eq!(
            entries
                .into_iter()
                .map(|entry| (entry.role, entry.text))
                .collect::<Vec<_>>(),
            vec![
                ("user".to_string(), "one".to_string()),
                ("assistant".to_string(), "two".to_string()),
                ("user".to_string(), "three".to_string())
            ]
        );
    }

    #[test]
    fn parses_codex_message_string_content() {
        let entries =
            parse_codex_history_from_jsonl(r#"{"type":"message","role":"user","content":"hi"}"#);

        assert_eq!(
            entry_pairs(entries),
            vec![("user".to_string(), "hi".to_string())]
        );
    }

    #[test]
    fn parses_codex_message_array_text_blocks() {
        let entries = parse_codex_history_from_jsonl(
            r#"{"type":"message","role":"assistant","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#,
        );

        assert_eq!(
            entry_pairs(entries),
            vec![("assistant".to_string(), "a\nb".to_string())]
        );
    }

    #[test]
    fn skips_codex_non_message_lines() {
        let entries = parse_codex_history_from_jsonl(
            r#"{"type":"session_header","role":"user","content":"skip"}
{"type":"message","role":"assistant","content":"keep"}"#,
        );

        assert_eq!(
            entry_pairs(entries),
            vec![("assistant".to_string(), "keep".to_string())]
        );
    }

    #[test]
    fn skips_codex_empty_text_after_trim() {
        let entries = parse_codex_history_from_jsonl(
            r#"{"type":"message","role":"user","content":"   "}
{"kind":"message","role":"assistant","content":[{"type":"text","text":"\n"}]}"#,
        );

        assert!(entries.is_empty());
    }

    #[test]
    fn find_claude_session_path_locates_file_in_first_matching_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let uuid = "claude-session";
        write_claude_session(temp.path(), "proj-a", uuid, "{}");

        let path = find_claude_session_path(temp.path(), uuid);

        assert_eq!(
            path,
            Some(
                temp.path()
                    .join(".claude/projects/proj-a")
                    .join(format!("{uuid}.jsonl"))
            )
        );
    }

    #[test]
    fn find_claude_session_path_returns_none_when_uuid_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_claude_session(temp.path(), "proj-a", "other-session", "{}");

        let path = find_claude_session_path(temp.path(), "missing-session");

        assert_eq!(path, None);
    }

    #[test]
    fn find_codex_session_path_locates_jsonl_under_year_month_day() {
        let temp = tempfile::tempdir().expect("tempdir");
        let uuid = "codex-session";
        let expected = write_codex_session(temp.path(), uuid, "{}");

        let path = find_codex_session_path(temp.path(), uuid);

        assert_eq!(path, Some(expected));
    }

    #[test]
    fn find_codex_session_path_returns_none_when_uuid_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_codex_session(temp.path(), "other-session", "{}");

        let path = find_codex_session_path(temp.path(), "missing-session");

        assert_eq!(path, None);
    }

    #[test]
    fn reads_claude_history_when_only_claude_path_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_claude_session(
            temp.path(),
            "-Volumes-Projects-spur",
            "claude-session",
            r#"{"type":"user","message":{"content":"claude prompt"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"claude answer"}]}}"#,
        );

        let entries = with_home(temp.path(), || history_entries("claude-session"));

        assert_eq!(
            entries,
            vec![
                ("user".to_string(), "claude prompt".to_string()),
                ("assistant".to_string(), "claude answer".to_string())
            ]
        );
    }

    #[test]
    fn reads_codex_history_when_only_codex_path_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_codex_session(
            temp.path(),
            "codex-session",
            r#"{"type":"session_header","content":"skip"}
{"kind":"message","role":"user","content":"codex prompt"}
{"type":"message","role":"assistant","content":[{"type":"text","text":"codex answer"}]}"#,
        );

        let entries = with_home(temp.path(), || history_entries("codex-session"));

        assert_eq!(
            entries,
            vec![
                ("user".to_string(), "codex prompt".to_string()),
                ("assistant".to_string(), "codex answer".to_string())
            ]
        );
    }
}
