//! `.spur/session_metadata.json` — persistent per-session metadata.
//!
//! Tracks title overrides, drafts, pin/archive state, and last-active pointer
//! for auto-resume. Writes are atomic (tmp-rename) to survive process crashes
//! and partial writes.
//!
//! Note: this relies on POSIX rename semantics (macOS/Linux) and guards only
//! against process crashes and partial writes. Durability across power loss
//! is not guaranteed — that would require `fsync` on the tmp file and on the
//! parent directory after rename.

use anyhow::{Context, Result};
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

    /// Clear the last-active pointer (used after an auto-resume banner has
    /// been shown so a subsequent session spawn in the same run doesn't
    /// re-trigger the banner).
    pub fn clear_last_active(&mut self) {
        self.metadata.last_active_session_id = None;
        self.metadata.last_active_at = None;
    }

    /// Remove entries for sessions no longer present in `live_ids`. If the
    /// `last_active_session_id` points to a removed entry, clear it too.
    /// Returns the session ids that were removed.
    pub fn gc_orphans(&mut self, live_ids: &[String]) -> Vec<String> {
        let live: std::collections::HashSet<&str> =
            live_ids.iter().map(|s| s.as_str()).collect();
        let to_remove: Vec<String> = self
            .metadata
            .sessions
            .keys()
            .filter(|k| !live.contains(k.as_str()))
            .cloned()
            .collect();
        for id in &to_remove {
            self.metadata.sessions.remove(id);
        }
        if let Some(ref last) = self.metadata.last_active_session_id {
            if !live.contains(last.as_str()) {
                self.metadata.last_active_session_id = None;
                self.metadata.last_active_at = None;
            }
        }
        to_remove
    }

    /// Atomic save: write to `path.tmp`, then rename to `path`. Creates parent
    /// directory if missing. Survives process crashes and partial writes via
    /// POSIX rename semantics (macOS/Linux). Does not guarantee durability
    /// across power loss (no `fsync` on tmp file or parent directory).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent directory {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.metadata)
            .context("serializing session metadata to JSON")?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .with_context(|| format!("writing tmp metadata file {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "renaming {} -> {}",
                tmp.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }
}
