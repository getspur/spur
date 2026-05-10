use std::path::PathBuf;

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

        build_connection_from_transport(config, args, perm_tx)
    }

    pub(super) async fn list_sessions_from_rpc(
        conn: &mut dyn AgentConnection,
    ) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();
        let mut cursor: Option<String> = None;

        for page_index in 0..MAX_SESSION_LIST_PAGES {
            // Some agents treat `cwd` as an exact match, which hides sessions
            // rooted in repo-owned worktrees. Fetch broadly and apply the
            // repo-prefix filter before emitting to the TUI.
            let list_req = ListSessionsRequest::new()
                .cwd(None::<PathBuf>)
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

    /// Read conversation history from a kiro session's JSONL file on disk.
    /// Returns (role, text) pairs for Prompt and AssistantMessage entries.
    pub(super) fn read_session_history_from_disk(
        session_uuid: &str,
    ) -> Vec<spur_acp::HistoryEntry> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let jsonl_path = home.join(format!(".kiro/sessions/cli/{}.jsonl", session_uuid));

        let content = match std::fs::read_to_string(&jsonl_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

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
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new_with_kind(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
            config.kind,
            permission_tx,
        )),
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
