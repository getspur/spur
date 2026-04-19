use serde::{Deserialize, Serialize};
use spur_acp::ContentBlock;

use crate::components::input_bar::ProtectedRange;

/// Maximum number of submitted-input entries retained in both the in-memory
/// `InputBar` ring buffer and the persisted `SessionMetadata::input_history`
/// vector. Single source of truth — do not redefine this number elsewhere.
pub const HISTORY_CAP: usize = 100;

/// Exact restorable input state for the composer.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct InputStateSnapshot {
    pub text: String,
    pub protected_ranges: Vec<ProtectedRange>,
}

#[derive(Deserialize)]
struct RawInputStateSnapshot {
    #[serde(default)]
    text: String,
    #[serde(default)]
    protected_ranges: Vec<ProtectedRange>,
}

impl<'de> Deserialize<'de> for InputStateSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawInputStateSnapshot::deserialize(deserializer)?;
        Ok(Self::sanitized(raw.text, raw.protected_ranges))
    }
}

impl InputStateSnapshot {
    pub fn new(text: String, protected_ranges: Vec<ProtectedRange>) -> Self {
        Self::sanitized(text, protected_ranges)
    }

    /// Build a snapshot, dropping any `ProtectedRange` that is out of
    /// bounds, off a UTF-8 char boundary, has `start > end`, or overlaps
    /// an earlier kept range. Keeps the text intact. Defense-in-depth for
    /// hand-edited or corrupted persisted history.
    fn sanitized(text: String, ranges: Vec<ProtectedRange>) -> Self {
        let mut sorted = ranges;
        sorted.sort_by_key(|r| r.start);

        let mut kept: Vec<ProtectedRange> = Vec::with_capacity(sorted.len());
        let mut last_end: usize = 0;
        for r in sorted {
            let in_bounds = r.start <= r.end && r.end <= text.len();
            let on_boundaries =
                text.is_char_boundary(r.start) && text.is_char_boundary(r.end);
            let non_overlapping = r.start >= last_end;
            if in_bounds && on_boundaries && non_overlapping {
                last_end = r.end;
                kept.push(r);
            }
        }

        Self {
            text,
            protected_ranges: kept,
        }
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self::new(text.into(), Vec::new())
    }

    /// Rebuild a restorable input snapshot from outbound content blocks.
    pub fn from_blocks(blocks: &[ContentBlock]) -> Self {
        let mut text = String::new();
        let mut protected_ranges = Vec::new();

        for block in blocks {
            match block {
                ContentBlock::Text(t) => text.push_str(&t.text),
                ContentBlock::ResourceLink(r) => {
                    let start = text.len();
                    text.push('@');
                    text.push_str(&r.name);
                    let end = text.len();
                    protected_ranges.push(ProtectedRange {
                        start,
                        end,
                        uri: r.uri.clone(),
                        name: r.name.clone(),
                    });
                }
                _ => {}
            }
        }

        Self {
            text,
            protected_ranges,
        }
    }
}

/// Persisted input-history entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputHistoryEntry {
    #[serde(flatten)]
    pub snapshot: InputStateSnapshot,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

impl InputHistoryEntry {
    pub fn new(snapshot: InputStateSnapshot) -> Self {
        Self {
            snapshot,
            submitted_at: None,
            session_id: None,
            agent: None,
        }
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self::new(InputStateSnapshot::from_text(text))
    }

    pub fn from_blocks(blocks: &[ContentBlock]) -> Self {
        Self::new(InputStateSnapshot::from_blocks(blocks))
    }

    pub fn with_context(
        mut self,
        submitted_at: Option<String>,
        session_id: Option<String>,
        agent: Option<String>,
    ) -> Self {
        self.submitted_at = submitted_at;
        self.session_id = session_id;
        self.agent = agent;
        self
    }

    pub fn same_recall_state(&self, other: &Self) -> bool {
        self.snapshot == other.snapshot
    }
}
