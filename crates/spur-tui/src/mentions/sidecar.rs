//! TUI mention sidecar (2026-08-27 spec §2 "TUI sidecars", decoupling spec
//! §4.2).
//!
//! When a TUI mention row is mapped onto a neutral
//! [`spur_mentions::MentionEntry`], everything the neutral type deliberately
//! does not model — worker composition state, [`AgentKind`], CLI identity,
//! issue previews, tags, section headers, and InputBar atom state — is
//! retained here, keyed by the neutral row's [`MentionId`]. The pair
//! (neutral entry, sidecar record) converts back to the TUI row without
//! loss; spur-tui is the only owner of these fields. Datasource prompt hints
//! and code payloads stay in their adapter-owned stores and are not part of
//! this mapping.

use std::collections::HashMap;
use std::sync::Arc;

use spur_acp::AgentKind;
use spur_mentions::{
    MentionEntry as NeutralMentionEntry, MentionId, MentionKind as NeutralMentionKind,
};

use super::entry::{MentionEntry, MentionKind};
use super::issue_source::IssueMentionDescriptor;

/// Caller-defined neutral categories the TUI maps its non-file kinds onto.
pub mod category {
    /// Composed-worker rows (`worker://…`).
    pub const WORKER: &str = "worker";
    /// Issue/work-item rows.
    pub const ISSUE: &str = "issue";
    /// Notebook datasource rows (`datasource://…`).
    pub const DATASOURCE: &str = "datasource";
}

/// Map a TUI row kind onto its neutral category.
///
/// File-tree and code-graph kinds map directly; the TUI-only kinds become
/// [`NeutralMentionKind::Custom`] categories so `spur-mentions` never names a
/// frontend's kinds. The inverse direction is not a function (foreign
/// categories have no TUI kind), which is why [`TuiMentionMetadata`] carries
/// the TUI kind itself.
pub fn neutral_kind(kind: &MentionKind) -> NeutralMentionKind {
    match kind {
        MentionKind::File => NeutralMentionKind::File,
        MentionKind::Directory => NeutralMentionKind::Directory,
        MentionKind::CodeFile => NeutralMentionKind::CodeFile,
        MentionKind::CodeSymbol => NeutralMentionKind::CodeSymbol,
        MentionKind::Worker => NeutralMentionKind::Custom(category::WORKER.into()),
        MentionKind::Issue => NeutralMentionKind::Custom(category::ISSUE.into()),
        MentionKind::Datasource => NeutralMentionKind::Custom(category::DATASOURCE.into()),
    }
}

/// TUI-only metadata for one mention row, retained when the row is mapped
/// onto a neutral `spur_mentions::MentionEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiMentionMetadata {
    /// The TUI row kind (the neutral kind only classifies it).
    pub kind: MentionKind,
    /// Optional synthetic section header marker (empty-query grouping rows).
    pub section_header: Option<&'static str>,
    /// Optional worker persona/profile selected by a composed worker mention.
    pub agent: Option<String>,
    /// Optional model selected by a composed worker mention.
    pub model: Option<String>,
    /// Optional effort selected by a composed worker mention.
    pub effort: Option<String>,
    /// Registered kind for worker mention rows.
    pub worker_kind: Option<AgentKind>,
    /// Command + effective args used to decide whether a cached catalog row
    /// is stale.
    pub worker_cli_identity: Option<String>,
    /// Optional relative path for code graph file and symbol entries.
    pub code_path: Option<String>,
    /// Optional enclosing scope for code graph symbol entries.
    pub code_scope: Option<String>,
    /// Optional right-aligned tag (worker tier, datasource kind label,
    /// issue priority).
    pub tag: Option<String>,
    /// Optional visible InputBar atom text, including the leading `@`.
    pub atom_text: Option<String>,
    /// Plain text that followed the consumed mention slots and must remain
    /// outside the protected atom when the picker accepts the row.
    pub unconsumed_suffix: Option<String>,
    /// Optional issue descriptor retained for richer issue-row previews.
    pub issue_preview: Option<Arc<IssueMentionDescriptor>>,
}

impl TuiMentionMetadata {
    /// Split a TUI row into its neutral core (stamped with `id`) plus the
    /// TUI-only metadata. Inverse of [`TuiMentionMetadata::rejoin`] for
    /// every TUI row: no field is dropped or duplicated.
    ///
    /// `insert_text` is always `None` on the neutral side: TUI rows derive
    /// their inserted text from `atom_text`/`display`, which the sidecar
    /// retains.
    pub fn split(id: MentionId, entry: &MentionEntry) -> (NeutralMentionEntry, Self) {
        let metadata = Self {
            kind: entry.kind.clone(),
            section_header: entry.section_header,
            agent: entry.agent.clone(),
            model: entry.model.clone(),
            effort: entry.effort.clone(),
            worker_kind: entry.worker_kind,
            worker_cli_identity: entry.worker_cli_identity.clone(),
            code_path: entry.code_path.clone(),
            code_scope: entry.code_scope.clone(),
            tag: entry.tag.clone(),
            atom_text: entry.atom_text.clone(),
            unconsumed_suffix: entry.unconsumed_suffix.clone(),
            issue_preview: entry.issue_preview.clone(),
        };
        let neutral = NeutralMentionEntry {
            id,
            kind: neutral_kind(&entry.kind),
            uri: entry.uri.clone(),
            display: entry.display.clone(),
            secondary: entry.secondary.clone(),
            search_text: entry.search_text.clone(),
            insert_text: None,
        };
        (neutral, metadata)
    }

    /// Rejoin a neutral core with its sidecar record into the TUI row.
    ///
    /// Exact inverse of [`TuiMentionMetadata::split`]: total and panic-free,
    /// including section-header rows (the TUI kind comes from the sidecar,
    /// so foreign neutral categories never mislabel a row). The neutral
    /// `insert_text` is ignored — TUI rows do not model it.
    pub fn rejoin(neutral: &NeutralMentionEntry, metadata: &Self) -> MentionEntry {
        MentionEntry {
            section_header: metadata.section_header,
            kind: metadata.kind.clone(),
            uri: neutral.uri.clone(),
            display: neutral.display.clone(),
            secondary: neutral.secondary.clone(),
            agent: metadata.agent.clone(),
            model: metadata.model.clone(),
            effort: metadata.effort.clone(),
            worker_kind: metadata.worker_kind,
            worker_cli_identity: metadata.worker_cli_identity.clone(),
            code_path: metadata.code_path.clone(),
            code_scope: metadata.code_scope.clone(),
            tag: metadata.tag.clone(),
            search_text: neutral.search_text.clone(),
            atom_text: metadata.atom_text.clone(),
            unconsumed_suffix: metadata.unconsumed_suffix.clone(),
            issue_preview: metadata.issue_preview.clone(),
        }
    }
}

/// Sidecar store keyed by the neutral row's [`MentionId`]: one metadata
/// record per mapped row.
pub type TuiMentionSidecar = HashMap<MentionId, TuiMentionMetadata>;
