//! Neutral mention core types (2026-08-27 mentions spec §2).
//!
//! Everything here is pure data: no ACP, `spur-core`, `spur-pm`, ratatui, or
//! TUI types cross this module. Frontend-specific row state stays behind a
//! sidecar keyed by [`MentionId`] (the TUI keeps
//! `spur_tui::mentions::sidecar::TuiMentionMetadata`).
//!
//! `CodeMentionPayload` is the one graph-backed export and is gated behind
//! the `code` feature in the crate root; the file-tree kinds below exist in
//! every feature configuration.

use std::fmt;
use std::path::Path;

/// Stable identifier of a neutral entry within one source snapshot.
///
/// Ids are minted sequentially by the source (or engine) that builds a
/// snapshot and are suitable as sidecar keys: they are `Copy`, totally
/// ordered, and hashable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MentionId(u64);

impl MentionId {
    /// Mint an id from its raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw id value (diagnostics, ordering).
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Successor id, for sequential minting while building a snapshot.
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Neutral entry category.
///
/// The file tree and the code-graph rows are engine-native; every other
/// consumer category (the TUI's worker/issue/datasource rows, the notebook's
/// rows) is expressed as [`MentionKind::Custom`] so this crate never names a
/// frontend's kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// File reachable through the code graph (`graph://file/…`).
    CodeFile,
    /// Symbol reachable through the code graph (`graph://symbol/…`).
    CodeSymbol,
    /// Caller-defined neutral category, e.g. `"worker"`, `"issue"`,
    /// `"datasource"`.
    Custom(Box<str>),
}

/// Mode-neutral ranking and insertion data for one mention candidate.
///
/// Section headers, worker agent/model/effort, `AgentKind`, CLI identity,
/// issue previews, tags, and `InputBar` atom state are deliberately absent:
/// they are frontend policy and live in a sidecar keyed by [`MentionId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionEntry {
    /// Stable within one source snapshot; the sidecar key.
    pub id: MentionId,
    /// File tree, code graph, or caller-defined category.
    pub kind: MentionKind,
    /// Canonical resource identity (`file://…`, `worker://…`, …).
    pub uri: String,
    /// Primary picker label.
    pub display: String,
    /// Optional neutral detail line.
    pub secondary: Option<String>,
    /// Explicit ranking haystack; `None` falls back to `display`.
    pub search_text: Option<String>,
    /// Text the consumer inserts on accept; `None` means "insert `display`".
    /// Range semantics stay with the consumer.
    pub insert_text: Option<String>,
}

/// Result of one [`MentionSource::build`]: neutral entries plus the snapshot's
/// monotonically increasing generation (2026-08-27 spec §4: publication is
/// atomic and generations never go backwards).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    /// Neutral candidate rows in source order.
    pub entries: Vec<MentionEntry>,
    /// Monotonically increasing snapshot generation.
    pub generation: u64,
}

impl SourceSnapshot {
    /// Assemble a snapshot from `entries` stamped with `generation`.
    pub fn new(entries: Vec<MentionEntry>, generation: u64) -> Self {
        Self {
            entries,
            generation,
        }
    }
}

/// Query-independent inputs to a [`MentionSource::build`].
///
/// Reserved for the M2 extraction: the filesystem traversal profile
/// (hidden/directory/ignore-rule switches that also fingerprint the cache
/// key) arrives here, so `build` never needs a per-frontend signature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceContext;

/// Typed failure of one source's [`MentionSource::build`].
///
/// Source-build failures are attributable to exactly one source key and are
/// never flattened into an empty picker (2026-08-27 spec §Error handling);
/// required sources fail the query, optional ones degrade to diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBuildError {
    /// Stable diagnostic message. Paths stay caller-redacted.
    pub message: String,
}

impl SourceBuildError {
    /// Build the error from a diagnostic message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceBuildError {}

/// A snapshot-producing mention source.
///
/// Object-safe: engines hold `Box<dyn MentionSource>` collections. The only
/// required surface is [`MentionSource::key`] (cache identity and
/// diagnostics) and [`MentionSource::build`]; everything else a frontend's
/// source does today (code payload hydration, datasource prompt hints) stays
/// in frontend-owned adapters until the later extraction phases.
pub trait MentionSource: Send {
    /// Stable key identifying this source in cache identity and diagnostics.
    fn key(&self) -> &str;

    /// Rebuild the snapshot from scratch for `root` under `context`.
    fn build(
        &mut self,
        root: &Path,
        context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_id_mints_sequentially() {
        let mut id = MentionId::new(0);
        assert_eq!(id.as_u64(), 0);
        id = id.next();
        assert_eq!(id, MentionId::new(1));
    }

    #[test]
    fn snapshot_new_stamps_generation() {
        let snapshot = SourceSnapshot::new(Vec::new(), 4);
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.generation, 4);
    }

    #[test]
    fn source_build_error_message_round_trips() {
        let error = SourceBuildError::new("no root");
        assert_eq!(error.message, "no root");
        assert_eq!(error.to_string(), "no root");
    }
}
