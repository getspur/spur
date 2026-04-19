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
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::input_history::InputHistoryEntry;

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
    /// Agent-authoritative ACP session id. `None` for entries written
    /// before this field was introduced (migrated silently via serde default).
    #[serde(default)]
    pub acp_session_id: Option<String>,
    /// Brain agent that owns `acp_session_id`. Used at resume time to
    /// avoid sending an ACP id to a different agent.
    #[serde(default)]
    pub brain_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub last_active_session_id: Option<String>,
    #[serde(default)]
    pub last_active_at: Option<String>,
    /// Mirror of the most recent `AgentSessionReady.acp_session_id`.
    /// Passed to `UserInput::ResumeSession` at next launch.
    #[serde(default)]
    pub last_active_acp_session_id: Option<String>,
    /// Mirror of the most recent `AgentSessionReady.brain`. Used to
    /// skip auto-resume when the launch-time `--brain` override does
    /// not match (avoids sending a claude id to kiro, etc.).
    #[serde(default)]
    pub last_active_brain: Option<String>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionEntry>,
    /// Global input history across all sessions, newest last.
    /// Persisted as exact input snapshots so recall preserves mention atoms.
    #[serde(default, deserialize_with = "deserialize_input_history")]
    pub input_history: Vec<InputHistoryEntry>,
}

fn default_version() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredInputHistoryEntry {
    Structured(InputHistoryEntry),
    Legacy(String),
}

fn deserialize_input_history<'de, D>(deserializer: D) -> Result<Vec<InputHistoryEntry>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Vec::<StoredInputHistoryEntry>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|entry| match entry {
            StoredInputHistoryEntry::Structured(entry) => entry,
            StoredInputHistoryEntry::Legacy(text) => InputHistoryEntry::from_text(text),
        })
        .collect())
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

    pub fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
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

    /// Clear every `last_active_*` pointer — including the ACP id and
    /// brain that drive `spur-cli`'s auto-resume gate. Used by the TUI
    /// `BrainRetired` arm so that `/clear` followed by a process quit
    /// before the next prompt does not auto-resume the just-retired
    /// session on the next launch.
    ///
    /// Distinct from [`clear_last_active`] which nulls only the
    /// TUI-local pointers (`last_active_session_id` + `last_active_at`);
    /// that method is used to suppress the one-shot resume banner and
    /// must not disable auto-resume.
    pub fn clear_last_active_full(&mut self) {
        self.metadata.last_active_session_id = None;
        self.metadata.last_active_at = None;
        self.metadata.last_active_acp_session_id = None;
        self.metadata.last_active_brain = None;
    }

    /// Remove entries for sessions no longer present in `live_ids`. If the
    /// `last_active_session_id` points to a removed entry, clear it too.
    /// Returns the session ids that were removed.
    pub fn gc_orphans(&mut self, live_ids: &[String]) -> Vec<String> {
        let live: std::collections::HashSet<&str> = live_ids.iter().map(|s| s.as_str()).collect();
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

    /// Persist the `(spur_id → acp_id, brain)` mapping on the per-entry
    /// record AND unconditionally promote to the top-level `last_active_*`
    /// pointers. See design doc: `AgentSessionReady` is the "newest live
    /// target to resume" signal, so mirroring is always correct.
    pub fn set_acp_mapping(&mut self, spur_id: &str, acp_id: &str, brain: &str) {
        let entry = self
            .metadata
            .sessions
            .entry(spur_id.to_string())
            .or_default();
        entry.acp_session_id = Some(acp_id.to_string());
        entry.brain_name = Some(brain.to_string());

        self.metadata.last_active_session_id = Some(spur_id.to_string());
        self.metadata.last_active_acp_session_id = Some(acp_id.to_string());
        self.metadata.last_active_brain = Some(brain.to_string());
    }

    /// Return the top-level `(acp_session_id, brain_name)` pair if both
    /// are populated. Used by `spur-cli watch` at startup.
    pub fn last_active_acp(&self) -> Option<(String, String)> {
        let acp = self.metadata.last_active_acp_session_id.clone()?;
        let brain = self.metadata.last_active_brain.clone()?;
        Some((acp, brain))
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
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod acp_mapping_tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn set_acp_mapping_populates_entry_and_top_level() {
        let tmp = NamedTempFile::new().unwrap();
        let mut store = SessionMetadataStore::load(tmp.path());

        store.set_acp_mapping("spur-abc", "acp-xyz", "claude-code-acp");

        let entry = store.entry("spur-abc").expect("entry created");
        assert_eq!(entry.acp_session_id.as_deref(), Some("acp-xyz"));
        assert_eq!(entry.brain_name.as_deref(), Some("claude-code-acp"));

        let (acp, brain) = store.last_active_acp().expect("top-level populated");
        assert_eq!(acp, "acp-xyz");
        assert_eq!(brain, "claude-code-acp");
    }

    #[test]
    fn last_active_acp_returns_none_when_absent() {
        let tmp = NamedTempFile::new().unwrap();
        let store = SessionMetadataStore::load(tmp.path());
        assert!(store.last_active_acp().is_none());
    }

    #[test]
    fn roundtrip_preserves_acp_mapping() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let mut store = SessionMetadataStore::load(tmp.path());
            store.set_acp_mapping("spur-1", "acp-1", "brain-a");
            store.save().unwrap();
        }
        let reloaded = SessionMetadataStore::load(tmp.path());
        assert_eq!(
            reloaded
                .entry("spur-1")
                .and_then(|e| e.acp_session_id.clone()),
            Some("acp-1".into())
        );
        assert_eq!(
            reloaded.metadata().last_active_acp_session_id.as_deref(),
            Some("acp-1")
        );
        assert_eq!(
            reloaded.metadata().last_active_brain.as_deref(),
            Some("brain-a")
        );
    }
}
