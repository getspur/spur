//! Worker `@`-mention source. Emits one entry per known worker
//! agent. The snapshot is supplied at construction time and is
//! independent of `root`.
//!
//! sud-m3: implements the *neutral* [`spur_mentions::MentionSource`]
//! directly (the engine builds it without a TUI adapter). TUI-only row
//! state (registered kind, CLI identity, tier) rides the sidecar keyed by
//! `MentionId`, and every snapshot replace bumps the data-revision token
//! so the engine cache key invalidates immediately.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use spur_acp::AgentKind;
use spur_mentions::{
    MentionSource as NeutralMentionSource, SourceBuildError, SourceContext, SourceSnapshot,
};

use super::entry::{MentionEntry, MentionKind};
use super::session::SessionMentionSource;
use super::sidecar::{split_rows, TuiMentionSidecar};

#[derive(Debug, Clone)]
pub struct WorkerMentionDescriptor {
    /// Unique slug, e.g. `"claude-code"`.
    pub name: String,
    /// Registered agent kind used to filter compatible `.spur/agents` profiles.
    pub kind: AgentKind,
    /// Command + effective args stamped into the model catalog cache.
    pub cli_identity: String,
    /// `delegation.description` from the agent config; shown as the
    /// row's `secondary` label in the picker.
    pub description: Option<String>,
    /// `"specialist"` or `"generalist"`; rendered as `⟨specialist⟩`.
    pub tier: Option<String>,
}

pub struct WorkerMentionSource {
    snapshot: Vec<WorkerMentionDescriptor>,
    /// Data-revision token participating in the engine cache key: bumped by
    /// every snapshot replace so the next query rebuilds instead of serving
    /// the retained snapshot until the TTL expires.
    token: u64,
    /// Sidecar records from the most recent build, keyed by row id.
    sidecar: TuiMentionSidecar,
}

impl WorkerMentionSource {
    pub fn new(snapshot: Vec<WorkerMentionDescriptor>) -> Self {
        Self::with_token(snapshot, 0)
    }

    /// Construct with an explicit data-revision token; the registry stamps
    /// `previous + 1` when swapping a session source so cache identity
    /// changes on every replace.
    pub(crate) fn with_token(snapshot: Vec<WorkerMentionDescriptor>, token: u64) -> Self {
        Self {
            snapshot,
            token,
            sidecar: TuiMentionSidecar::new(),
        }
    }

    /// Replace the worker snapshot in place, bumping the data-revision
    /// token.
    pub fn set_snapshot(&mut self, snapshot: Vec<WorkerMentionDescriptor>) {
        self.snapshot = snapshot;
        self.token = self.token.wrapping_add(1);
    }

    /// TUI rows for the current snapshot: the pre-sidecar shape the neutral
    /// entries and the sidecar records split from.
    fn tui_rows(&self) -> Vec<MentionEntry> {
        self.snapshot
            .iter()
            .map(|d| MentionEntry {
                section_header: None,
                kind: MentionKind::Worker,
                uri: format!("worker://{}", d.name),
                display: format!("worker:{}", d.name),
                secondary: d.description.clone(),
                agent: None,
                model: None,
                effort: None,
                worker_kind: Some(d.kind),
                worker_cli_identity: Some(d.cli_identity.clone()),
                code_path: None,
                code_scope: None,
                tag: d.tier.clone(),
                search_text: None,
                atom_text: None,
                unconsumed_suffix: None,
                issue_preview: None,
            })
            .collect()
    }
}

impl NeutralMentionSource for WorkerMentionSource {
    fn key(&self) -> &str {
        "worker"
    }

    fn source_token(&self) -> u64 {
        self.token
    }

    fn build(
        &mut self,
        _root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        let rows = self.tui_rows();
        let (entries, sidecar) = split_rows(&rows);
        self.sidecar = sidecar;
        Ok(SourceSnapshot::new(entries, 0))
    }
}

impl SessionMentionSource for WorkerMentionSource {
    fn slot_name(&self) -> &'static str {
        "worker"
    }

    fn sidecar(&self) -> &TuiMentionSidecar {
        &self.sidecar
    }

    fn datasource_hints(&self) -> Option<&HashMap<String, Arc<String>>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_mentions::MentionId;

    fn descriptor(name: &str, desc: Option<&str>, tier: Option<&str>) -> WorkerMentionDescriptor {
        WorkerMentionDescriptor {
            name: name.into(),
            kind: AgentKind::from_name(name),
            cli_identity: name.into(),
            description: desc.map(str::to_string),
            tier: tier.map(str::to_string),
        }
    }

    fn build_neutral(source: &mut WorkerMentionSource) -> SourceSnapshot {
        source
            .build(Path::new("/"), &SourceContext::default())
            .expect("build ok")
    }

    #[test]
    fn token_changes_when_the_snapshot_is_replaced() {
        // sud-m3 RED-first pin (source-level view of the registry test):
        // the data-revision token feeding the engine cache key must move on
        // every snapshot replace.
        let mut src = WorkerMentionSource::new(vec![descriptor("codex", None, None)]);
        let first = NeutralMentionSource::source_token(&src);
        src.set_snapshot(vec![descriptor("gemini", None, None)]);
        let second = NeutralMentionSource::source_token(&src);
        assert_ne!(first, second);
        src.set_snapshot(vec![descriptor("codex", None, None)]);
        assert_ne!(
            second,
            NeutralMentionSource::source_token(&src),
            "repeated swaps must not alias back to an earlier token"
        );
    }

    #[test]
    fn build_emits_neutral_entries_that_rejoin_into_tui_rows() {
        let mut src = WorkerMentionSource::new(vec![
            descriptor("claude-code", Some("Refactors Rust"), Some("specialist")),
            descriptor("codex", Some("Writes tests"), Some("generalist")),
            descriptor("kiro", None, None),
        ]);
        let expected = src.tui_rows();

        let snapshot = build_neutral(&mut src);

        assert_eq!(snapshot.entries.len(), 3);
        for (index, entry) in snapshot.entries.iter().enumerate() {
            assert_eq!(entry.id, MentionId::new(index as u64));
            assert_eq!(entry.uri, format!("worker://{}", src.snapshot[index].name));
            assert_eq!(
                entry.display,
                format!("worker:{}", src.snapshot[index].name)
            );
            assert_eq!(entry.secondary, src.snapshot[index].description);
        }
        // The sidecar is written by the same build; rejoining is lossless.
        let rejoined = super::super::sidecar::rejoin_snapshot(&snapshot.entries, src.sidecar());
        assert_eq!(rejoined, expected);
        assert_eq!(rejoined[0].kind, MentionKind::Worker);
        assert_eq!(
            rejoined[0].worker_kind,
            Some(AgentKind::from_name("claude-code"))
        );
        assert_eq!(rejoined[0].tag.as_deref(), Some("specialist"));
        assert_eq!(rejoined[2].secondary, None);
        assert_eq!(rejoined[2].tag, None);
    }

    #[test]
    fn key_and_slot_name_are_worker() {
        let src = WorkerMentionSource::new(vec![]);
        assert_eq!(NeutralMentionSource::key(&src), "worker");
        assert_eq!(SessionMentionSource::slot_name(&src), "worker");
    }
}
