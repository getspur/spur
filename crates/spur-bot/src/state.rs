use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBotState {
    pub version: u32,
    pub operator_chat_id: Option<i64>,
    pub current_acp_session_id: Option<String>,
    pub current_brain: Option<String>,
}

impl Default for PersistedBotState {
    fn default() -> Self {
        Self {
            version: 1,
            operator_chat_id: None,
            current_acp_session_id: None,
            current_brain: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingState {
    NoSession,
    RestorePending {
        acp_session_id: String,
        brain: String,
    },
    Active {
        session: spur_acp::SessionId,
        acp_session_id: String,
        brain: String,
    },
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
        let raw = std::fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, state: &PersistedBotState) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }
}
