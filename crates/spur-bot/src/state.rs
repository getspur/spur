use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
}

impl ThreadKey {
    pub fn lobby(chat_id: i64) -> Self {
        Self {
            chat_id,
            message_thread_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingState {
    Unbound,
    RestorePending {
        acp_session_id: String,
        brain: String,
    },
    Active {
        #[serde(skip)]
        session: spur_acp::SessionId,
        acp_session_id: String,
        brain: String,
    },
    ArchivedDetached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedThreadRecord {
    pub topic_name: String,
    pub archived: bool,
    pub acp_session_id: Option<String>,
    pub brain: Option<String>,
    pub binding_state: BindingState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBotState {
    pub version: u32,
    pub operator_chat_id: Option<i64>,
    pub next_topic_seq: u32,
    pub threads: HashMap<i32, PersistedThreadRecord>,
}

impl Default for PersistedBotState {
    fn default() -> Self {
        Self {
            version: 2,
            operator_chat_id: None,
            next_topic_seq: 1,
            threads: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LegacyPersistedBotState {
    version: u32,
    operator_chat_id: Option<i64>,
    current_acp_session_id: Option<String>,
    current_brain: Option<String>,
}

pub struct BotStateStore {
    path: PathBuf,
}

impl BotStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> anyhow::Result<PersistedBotState> {
        if !self.path.exists() {
            return Ok(PersistedBotState::default());
        }

        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading state file {}", self.path.display()))?;
        if let Ok(state) = serde_json::from_str::<PersistedBotState>(&raw) {
            return Ok(state);
        }

        let legacy: LegacyPersistedBotState = serde_json::from_str(&raw).with_context(|| {
            format!(
                "parsing state file {} (current and legacy schemas both failed)",
                self.path.display()
            )
        })?;
        let mut migrated = PersistedBotState {
            operator_chat_id: legacy.operator_chat_id,
            ..PersistedBotState::default()
        };

        if let (Some(acp_session_id), Some(brain)) =
            (legacy.current_acp_session_id, legacy.current_brain)
        {
            migrated.threads.insert(
                -1,
                PersistedThreadRecord {
                    topic_name: "Legacy Session".into(),
                    archived: true,
                    acp_session_id: Some(acp_session_id.clone()),
                    brain: Some(brain.clone()),
                    binding_state: BindingState::ArchivedDetached,
                },
            );
        }

        Ok(migrated)
    }

    pub fn save(&self, state: &PersistedBotState) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state parent dir {}", parent.display()))?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(state)?)
            .with_context(|| format!("writing state file {}", self.path.display()))?;
        Ok(())
    }
}
