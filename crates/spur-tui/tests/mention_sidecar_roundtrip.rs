//! sud-m1 lossless round-trip gate: every TUI mention row must survive the
//! mapping onto (`spur_mentions::MentionEntry`, `TuiMentionMetadata`) and
//! back, byte for byte, for file, worker, issue, datasource, and code rows
//! plus synthetic section-header rows (2026-08-27 spec §2 "TUI sidecars",
//! decoupling spec §5 Phase M1). The sidecar is keyed by `MentionId`.

use std::sync::Arc;

use spur_mentions::{MentionId, MentionKind as NeutralMentionKind};
use spur_tui::mentions::{
    sidecar, IssueMentionDescriptor, MentionEntry, MentionKind, TuiMentionMetadata,
    TuiMentionSidecar,
};

fn file_entry() -> MentionEntry {
    MentionEntry {
        section_header: None,
        kind: MentionKind::File,
        uri: "file:///work/crates/spur-tui/src/main.rs".to_string(),
        display: "src/main.rs".to_string(),
        secondary: None,
        agent: None,
        model: None,
        effort: None,
        worker_kind: None,
        worker_cli_identity: None,
        code_path: None,
        code_scope: None,
        tag: None,
        search_text: None,
        atom_text: None,
        unconsumed_suffix: None,
        issue_preview: None,
    }
}

fn worker_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::Worker,
        uri: "worker://claude-code".to_string(),
        display: "worker:claude-code".to_string(),
        secondary: Some("characterization worker".to_string()),
        // Composed worker rows carry the picked persona/model/effort.
        agent: Some("explorer".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        effort: Some("high".to_string()),
        worker_kind: Some(spur_acp::AgentKind::ClaudeCodeAcp),
        worker_cli_identity: Some("claude --acp".to_string()),
        tag: Some("specialist".to_string()),
        search_text: Some("claude-code explorer claude-sonnet-4-6".to_string()),
        atom_text: Some("@claude-code:explorer:claude-sonnet-4-6:high".to_string()),
        unconsumed_suffix: Some(" please review".to_string()),
        ..file_entry()
    }
}

fn issue_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::Issue,
        uri: "https://example.test/ISSUE-12".to_string(),
        display: "ISSUE-12: fix picker ordering".to_string(),
        secondary: Some("in_progress · kevin".to_string()),
        tag: Some("P2".to_string()),
        search_text: Some("ISSUE-12 fix picker ordering ux".to_string()),
        atom_text: Some("@ISSUE-12".to_string()),
        issue_preview: Some(Arc::new(IssueMentionDescriptor {
            id: "ISSUE-12".to_string(),
            title: "fix picker ordering".to_string(),
            source: spur_pm::PmSource::Beads,
            status: "in_progress".to_string(),
            assignee: Some("kevin".to_string()),
            priority: Some(2),
            issue_type: Some("task".to_string()),
            labels: vec!["ux".to_string()],
            url: "https://example.test/ISSUE-12".to_string(),
            description: None,
        })),
        ..file_entry()
    }
}

fn datasource_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::Datasource,
        uri: "datasource://line_items".to_string(),
        display: "line_items".to_string(),
        secondary: Some("csv · 10 rows".to_string()),
        tag: Some("csv".to_string()),
        search_text: Some("line_items csv".to_string()),
        atom_text: Some("@line_items".to_string()),
        ..file_entry()
    }
}

fn code_file_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::CodeFile,
        uri: "graph://file/src/mentions/registry.rs".to_string(),
        display: "src/mentions/registry.rs".to_string(),
        code_path: Some("src/mentions/registry.rs".to_string()),
        search_text: Some("src/mentions/registry.rs".to_string()),
        ..file_entry()
    }
}

fn code_symbol_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::CodeSymbol,
        uri: "graph://symbol/src/mentions/registry.rs#MentionRegistry".to_string(),
        display: "MentionRegistry".to_string(),
        secondary: Some("struct".to_string()),
        code_path: Some("src/mentions/registry.rs".to_string()),
        code_scope: Some("spur_tui::mentions".to_string()),
        atom_text: Some("@MentionRegistry".to_string()),
        ..file_entry()
    }
}

fn directory_entry() -> MentionEntry {
    MentionEntry {
        kind: MentionKind::Directory,
        uri: "file:///work/crates/spur-tui/src/mentions/".to_string(),
        display: "src/mentions/".to_string(),
        ..file_entry()
    }
}

fn section_header_entry() -> MentionEntry {
    MentionEntry {
        section_header: Some("Files"),
        ..file_entry()
    }
}

fn assert_roundtrip(id_value: u64, original: &MentionEntry) {
    let id = MentionId::new(id_value);
    let (neutral, metadata) = TuiMentionMetadata::split(id, original);
    assert_eq!(neutral.id, id, "neutral entry carries the minted id");

    let mut sidecar: TuiMentionSidecar = TuiMentionSidecar::new();
    sidecar.insert(neutral.id, metadata);
    let stored = sidecar
        .get(&id)
        .expect("sidecar is keyed by the neutral MentionId");

    let rebuilt = TuiMentionMetadata::rejoin(&neutral, stored);
    assert_eq!(
        &rebuilt, original,
        "TUI row must survive (neutral, sidecar) and back without loss"
    );
}

#[test]
fn sidecar_roundtrip_is_lossless_for_every_tui_row_kind() {
    assert_roundtrip(0, &file_entry());
    assert_roundtrip(1, &directory_entry());
    assert_roundtrip(2, &worker_entry());
    assert_roundtrip(3, &issue_entry());
    assert_roundtrip(4, &datasource_entry());
    assert_roundtrip(5, &code_file_entry());
    assert_roundtrip(6, &code_symbol_entry());
    assert_roundtrip(7, &section_header_entry());
}

#[test]
fn split_keeps_neutral_fields_neutral_and_tui_fields_in_the_sidecar() {
    let original = worker_entry();
    let (neutral, metadata) = TuiMentionMetadata::split(MentionId::new(11), &original);

    // Neutral core: ranking and insertion data only.
    assert_eq!(neutral.id, MentionId::new(11));
    assert_eq!(neutral.uri, "worker://claude-code");
    assert_eq!(neutral.display, "worker:claude-code");
    assert_eq!(
        neutral.secondary.as_deref(),
        Some("characterization worker")
    );
    assert_eq!(
        neutral.search_text.as_deref(),
        Some("claude-code explorer claude-sonnet-4-6")
    );
    // TUI rows never model `insert_text`; it stays reserved for consumers
    // that insert different text than they display.
    assert_eq!(neutral.insert_text, None);

    // Sidecar: worker composition state, tags, and InputBar atoms.
    assert_eq!(metadata.agent.as_deref(), Some("explorer"));
    assert_eq!(metadata.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(metadata.effort.as_deref(), Some("high"));
    assert_eq!(
        metadata.worker_kind,
        Some(spur_acp::AgentKind::ClaudeCodeAcp)
    );
    assert_eq!(
        metadata.worker_cli_identity.as_deref(),
        Some("claude --acp")
    );
    assert_eq!(metadata.tag.as_deref(), Some("specialist"));
    assert_eq!(
        metadata.atom_text.as_deref(),
        Some("@claude-code:explorer:claude-sonnet-4-6:high")
    );
    assert_eq!(
        metadata.unconsumed_suffix.as_deref(),
        Some(" please review")
    );
}

#[test]
fn split_carries_issue_previews_in_the_sidecar() {
    let original = issue_entry();
    let (neutral, metadata) = TuiMentionMetadata::split(MentionId::new(12), &original);
    assert_eq!(neutral.uri, "https://example.test/ISSUE-12");
    let preview = metadata
        .issue_preview
        .expect("issue preview stays TUI-side");
    assert_eq!(preview.id, "ISSUE-12");
    assert_eq!(preview.title, "fix picker ordering");
    assert_eq!(preview.priority, Some(2));
}

#[test]
fn split_carries_code_rows_without_graph_types_in_the_neutral_core() {
    let original = code_symbol_entry();
    let (neutral, metadata) = TuiMentionMetadata::split(MentionId::new(13), &original);
    assert_eq!(
        neutral.uri,
        "graph://symbol/src/mentions/registry.rs#MentionRegistry"
    );
    assert_eq!(
        metadata.code_path.as_deref(),
        Some("src/mentions/registry.rs")
    );
    assert_eq!(metadata.code_scope.as_deref(), Some("spur_tui::mentions"));
}

#[test]
fn neutral_kind_maps_every_tui_kind_without_loss() {
    let cases = [
        (MentionKind::File, NeutralMentionKind::File),
        (MentionKind::Directory, NeutralMentionKind::Directory),
        (MentionKind::CodeFile, NeutralMentionKind::CodeFile),
        (MentionKind::CodeSymbol, NeutralMentionKind::CodeSymbol),
        (
            MentionKind::Worker,
            NeutralMentionKind::Custom(sidecar::category::WORKER.into()),
        ),
        (
            MentionKind::Issue,
            NeutralMentionKind::Custom(sidecar::category::ISSUE.into()),
        ),
        (
            MentionKind::Datasource,
            NeutralMentionKind::Custom(sidecar::category::DATASOURCE.into()),
        ),
    ];
    for (tui_kind, expected) in cases {
        assert_eq!(sidecar::neutral_kind(&tui_kind), expected);
    }
}
