//! Owned sidebar chat state for the Tauri process.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use spur_acp::config::{AgentConfig, SpurConfig};
use spur_acp::connection::{
    AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter,
};
use spur_acp::types::{PermissionRequest, TransportKind};
use tokio::sync::{mpsc, Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use spur_notebook::sidebar_chat::manager::SidebarChat as SidebarChatManager;

/// Shared sidebar chat manager handle used by Tauri commands.
///
/// The manager already uses interior async mutexes. Keeping this handle
/// lock-free lets a streaming turn, permission pump, and permission response
/// command access the same pending-permission map concurrently.
pub type SidebarChatHandle = Arc<SidebarChatManager>;

/// Error returned when sidebar chat cannot be made available.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChatStateError {
    /// No ACP-capable agent is configured.
    #[error("sidebar chat unavailable: {reason}")]
    Unavailable {
        /// Human-readable reason suitable for logs and command error payloads.
        reason: String,
    },
}

/// Process-wide sidebar chat state owned by Tauri.
pub struct SidebarChatState {
    chat: OnceCell<SidebarChatHandle>,
    cancellation_root: CancellationToken,
    active_turns: Mutex<HashMap<String, CancellationToken>>,
    permission_tx: mpsc::UnboundedSender<PermissionRequest>,
    permission_rx: Mutex<mpsc::UnboundedReceiver<PermissionRequest>>,
    agent: Option<AgentConfig>,
    repo_root: PathBuf,
}

impl Default for SidebarChatState {
    fn default() -> Self {
        let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let config = load_spur_config(&repo_root);
        Self::from_config(config, repo_root)
    }
}

impl SidebarChatState {
    /// Construct state from an explicit config.
    pub fn from_config(config: SpurConfig, repo_root: PathBuf) -> Self {
        let (permission_tx, permission_rx) = mpsc::unbounded_channel();
        Self {
            chat: OnceCell::new(),
            cancellation_root: CancellationToken::new(),
            active_turns: Mutex::new(HashMap::new()),
            permission_tx,
            permission_rx: Mutex::new(permission_rx),
            agent: select_default_agent(&config),
            repo_root,
        }
    }

    /// Lazily create and return the sidebar chat manager.
    pub async fn chat(&self) -> Result<SidebarChatHandle, ChatStateError> {
        let agent = self
            .agent
            .clone()
            .ok_or_else(|| ChatStateError::Unavailable {
                reason: "no agent configured".to_owned(),
            })?;
        let repo_root = self.repo_root.clone();
        let permission_tx = self.permission_tx.clone();

        self.chat
            .get_or_try_init(|| async move {
                let connection = build_agent_connection(&agent, &repo_root, Some(permission_tx));
                let manager = SidebarChatManager::new(connection);
                Ok(Arc::new(manager))
            })
            .await
            .cloned()
    }

    /// Return the cancellation root for all sidebar chat work.
    pub fn cancellation_root(&self) -> CancellationToken {
        self.cancellation_root.clone()
    }

    /// Register a turn-specific cancellation token for a live ACP session.
    pub async fn register_turn_cancel(&self, session_id: String, token: CancellationToken) {
        self.active_turns.lock().await.insert(session_id, token);
    }

    /// Remove and return a live turn cancellation token, if one exists.
    pub async fn take_turn_cancel(&self, session_id: &str) -> Option<CancellationToken> {
        self.active_turns.lock().await.remove(session_id)
    }

    /// Forget a completed turn cancellation token.
    pub async fn unregister_turn_cancel(&self, session_id: &str) {
        self.active_turns.lock().await.remove(session_id);
    }

    /// Return a clone of the permission request sender wired into native ACP.
    pub fn permission_sender(&self) -> mpsc::UnboundedSender<PermissionRequest> {
        self.permission_tx.clone()
    }

    /// Lock the permission request receiver.
    pub async fn permission_receiver(
        &self,
    ) -> tokio::sync::MutexGuard<'_, mpsc::UnboundedReceiver<PermissionRequest>> {
        self.permission_rx.lock().await
    }

    #[cfg(test)]
    fn selected_agent_name(&self) -> Option<&str> {
        self.agent.as_ref().map(|agent| agent.name.as_str())
    }
}

/// Return the lazily initialized sidebar chat manager for a Tauri state object.
pub async fn get_or_init_sidebar_chat(
    state: &crate::state::State,
) -> Result<SidebarChatHandle, ChatStateError> {
    state.sidebar_chat.chat().await
}

/// Load layered SPUR config: project `.spur/config.toml`, then user
/// `~/.spur/config.toml`, falling back to defaults on absence or parse errors.
pub fn load_spur_config(repo_root: &Path) -> SpurConfig {
    let project_config = repo_root.join(".spur").join("config.toml");
    let user_config = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".spur/config.toml"))
        .unwrap_or_default();

    let path = if project_config.exists() {
        Some(project_config)
    } else if user_config.exists() {
        Some(user_config)
    } else {
        None
    };

    let Some(path) = path else {
        return SpurConfig::default();
    };

    match std::fs::read_to_string(&path).map(|content| toml::from_str::<SpurConfig>(&content)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            warn!(%error, path = %path.display(), "failed to parse SPUR config; using defaults");
            SpurConfig::default()
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to read SPUR config; using defaults");
            SpurConfig::default()
        }
    }
}

/// Pick the configured brain agent, then the first registered agent.
pub fn select_default_agent(config: &SpurConfig) -> Option<AgentConfig> {
    config
        .agents
        .entries
        .iter()
        .find(|agent| agent.name == config.brain.default)
        .or_else(|| config.agents.entries.first())
        .cloned()
}

/// Build the configured ACP connection and optionally wire native permissions.
pub fn build_agent_connection(
    config: &AgentConfig,
    repo_root: &Path,
    permission_tx: Option<mpsc::UnboundedSender<PermissionRequest>>,
) -> Arc<Mutex<dyn AgentConnection>> {
    let spawn_args = config.effective_args();
    match config.transport {
        TransportKind::Acp => {
            let mut connection = NativeAcpConnection::new_with_kind(
                config.name.clone(),
                config.command.clone(),
                spawn_args,
                config.kind,
                permission_tx,
            );
            connection.set_repo_root(repo_root.to_path_buf());
            Arc::new(Mutex::new(connection))
        }
        TransportKind::Stdio => Arc::new(Mutex::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        ))),
        TransportKind::CliWrap => Arc::new(Mutex::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        ))),
        TransportKind::StreamJson => Arc::new(Mutex::new(StreamJsonAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::config::{AgentConfig, AgentsConfig, BrainConfig, SpurConfig};

    #[tokio::test]
    async fn unavailable_when_config_has_no_agent() {
        let state = SidebarChatState::from_config(SpurConfig::default(), PathBuf::from("."));

        let error = match state.chat().await {
            Ok(_) => panic!("missing agent should return an unavailable error"),
            Err(error) => error,
        };

        assert!(matches!(error, ChatStateError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn selects_configured_agent_and_keeps_single_manager_cell() {
        let config = SpurConfig {
            brain: BrainConfig {
                default: "brain".to_owned(),
                ..BrainConfig::default()
            },
            agents: AgentsConfig {
                entries: vec![
                    AgentConfig {
                        transport: TransportKind::Stdio,
                        command: "first".to_owned(),
                        ..AgentConfig::with_defaults("first")
                    },
                    AgentConfig {
                        transport: TransportKind::Acp,
                        command: "brain".to_owned(),
                        ..AgentConfig::with_defaults("brain")
                    },
                ],
            },
            ..SpurConfig::default()
        };
        let state = SidebarChatState::from_config(config, PathBuf::from("."));

        let first = state.chat().await.unwrap();
        let second = state.chat().await.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(state.selected_agent_name(), Some("brain"));
    }

    #[tokio::test]
    async fn state_helper_returns_real_sidebar_chat_manager_type() {
        let config = SpurConfig {
            agents: AgentsConfig {
                entries: vec![AgentConfig {
                    transport: TransportKind::Stdio,
                    command: "agent".to_owned(),
                    ..AgentConfig::with_defaults("agent")
                }],
            },
            ..SpurConfig::default()
        };
        let mut state = crate::state::State::new();
        state.sidebar_chat = SidebarChatState::from_config(config, PathBuf::from("."));

        let chat: Arc<spur_notebook::sidebar_chat::manager::SidebarChat> =
            get_or_init_sidebar_chat(&state).await.unwrap();

        assert!(Arc::ptr_eq(
            &chat,
            &get_or_init_sidebar_chat(&state).await.unwrap()
        ));
    }

    #[test]
    fn exposes_permission_sender_and_cancellation_root() {
        let state = SidebarChatState::default();

        let _permission_tx = state.permission_sender();
        assert!(!state.cancellation_root().is_cancelled());
    }

    #[tokio::test]
    async fn tracks_turn_cancellation_tokens_by_session_id() {
        let state = SidebarChatState::default();
        let token = state.cancellation_root().child_token();

        state
            .register_turn_cancel("session-1".to_owned(), token.clone())
            .await;

        let stored = state.take_turn_cancel("session-1").await.unwrap();
        stored.cancel();
        assert!(token.is_cancelled());
        assert!(state.take_turn_cancel("session-1").await.is_none());
    }
}
