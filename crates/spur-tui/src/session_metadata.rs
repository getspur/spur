//! `.spur/session_metadata.json` — persistent per-session metadata.
//!
//! Tracks title overrides, drafts, pin/archive state, and last-active pointer
//! for auto-resume. Writes are atomic (tmp-rename) to survive crashes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub last_opened_at: String,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub last_active_session_id: Option<String>,
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionEntry>,
}

fn default_version() -> u32 {
    1
}

pub struct SessionMetadataStore {
    path: PathBuf,
    metadata: SessionMetadata,
}

impl SessionMetadataStore {
    /// Read the metadata file from `path`. Missing or malformed file → empty store.
    pub fn load(path: &Path) -> Self {
        let metadata = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionMetadata>(&s).ok())
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            metadata,
        }
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn entry(&self, session_id: &str) -> Option<&SessionEntry> {
        self.metadata.sessions.get(session_id)
    }

    pub fn entry_mut(&mut self, session_id: &str) -> &mut SessionEntry {
        self.metadata
            .sessions
            .entry(session_id.to_string())
            .or_default()
    }

    pub fn upsert_entry(&mut self, session_id: String, entry: SessionEntry) {
        self.metadata.sessions.insert(session_id, entry);
    }

    pub fn remove_entry(&mut self, session_id: &str) {
        self.metadata.sessions.remove(session_id);
    }

    pub fn set_last_active(&mut self, session_id: String, at: String) {
        self.metadata.last_active_session_id = Some(session_id);
        self.metadata.last_active_at = Some(at);
    }

    /// Atomic save: write to `path.tmp`, then rename to `path`. Creates parent
    /// directory if missing.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.metadata)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
