use spur_tui::commands::registry::CommandRegistry;
use spur_tui::components::palette::{PaletteKind, PalettePayload};
use spur_tui::components::palette_sources::{
    CommandSource, PaletteSource, SessionSource, WorkerSource,
};
use spur_tui::session_metadata::{SessionEntry, SessionMetadata, SessionMetadataStore};

#[test]
fn command_source_yields_all_registered_commands_as_command_kind() {
    let registry = CommandRegistry::default();
    let src = CommandSource::new(&registry);
    let results = src.collect();

    assert!(
        !results.is_empty(),
        "default registry should contain builtin commands"
    );
    assert!(results.iter().all(|r| r.kind == PaletteKind::Command));

    // Every result has a command payload with a non-empty name.
    for r in &results {
        match &r.payload {
            PalettePayload::Command { name } => assert!(!name.is_empty()),
            _ => panic!("expected Command payload, got {:?}", r.payload),
        }
    }
}

#[test]
fn session_source_yields_session_kind_rows_with_title_as_label() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "sess-1".to_string(),
        SessionEntry {
            title_override: Some("refactor-auth".to_string()),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "sess-2".to_string(),
        SessionEntry {
            title_override: None, // falls back to session_id as label
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    assert_eq!(results.len(), 2);

    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert!(labels.contains(&"refactor-auth"));
    assert!(labels.contains(&"sess-2"));

    for r in &results {
        assert!(matches!(
            r.kind,
            spur_tui::components::palette::PaletteKind::Session
        ));
    }
}

#[test]
fn session_source_uses_raw_acp_id_for_canonical_metadata_entry() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "0123456789abcdef".into(),
        SessionEntry {
            title_override: Some("mapped session".into()),
            acp_session_id: Some("raw-acp-session-id".into()),
            ..SessionEntry::default()
        },
    );

    let results = SessionSource::from_metadata(&meta).collect();

    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0].payload,
        PalettePayload::Session { session_id } if session_id == "raw-acp-session-id"
    ));
}

#[test]
fn migrated_draft_remains_resumable_before_agent_session_ready() {
    let raw_acp_id = "raw-acp-before-ready";
    let spur_id =
        spur_core::plan::labels::derive_brain_session_id(&spur_acp::SessionId(raw_acp_id.into()))
            .into_session_id();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut store = SessionMetadataStore::load(tmp.path());
    store.upsert_entry(
        raw_acp_id.into(),
        SessionEntry {
            title_override: Some("retry me".into()),
            draft: "keep draft".into(),
            ..SessionEntry::default()
        },
    );

    assert!(store.migrate_acp_entry(&spur_id.0, raw_acp_id));
    let migrated = store.entry(&spur_id.0).expect("canonical entry created");
    assert_eq!(migrated.acp_session_id.as_deref(), Some(raw_acp_id));

    let results = SessionSource::from_metadata(store.metadata()).collect();
    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0].payload,
        PalettePayload::Session { session_id } if session_id == raw_acp_id
    ));
}

#[test]
fn session_source_excludes_unmapped_derived_shaped_key() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "0123456789abcdef".into(),
        SessionEntry {
            title_override: Some("ambiguous identity".into()),
            ..SessionEntry::default()
        },
    );

    assert!(SessionSource::from_metadata(&meta).collect().is_empty());
}

use spur_core::lineage::projection::ExecutorLineage;

#[test]
fn worker_source_yields_no_rows_for_empty_lineage() {
    let lineage = ExecutorLineage::new();
    let src = WorkerSource::from_lineage(&lineage);
    assert_eq!(src.collect().len(), 0);
}

use spur_tui::components::palette_sources::TraceSource;

#[test]
fn trace_source_handles_empty_trace() {
    let src = TraceSource::from_empty();
    assert_eq!(src.collect().len(), 0);
}

#[test]
fn session_source_sorts_by_last_opened_at_descending() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "old".to_string(),
        SessionEntry {
            title_override: Some("old-session".to_string()),
            last_opened_at: "2020-01-01T00:00:00Z".to_string(),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "new".to_string(),
        SessionEntry {
            title_override: Some("new-session".to_string()),
            last_opened_at: "2026-04-20T12:00:00Z".to_string(),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "mid".to_string(),
        SessionEntry {
            title_override: Some("mid-session".to_string()),
            last_opened_at: "2023-06-15T08:30:00Z".to_string(),
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["new-session", "mid-session", "old-session"],
        "sessions should be ordered by last_opened_at descending"
    );
}

#[test]
fn session_source_sinks_empty_timestamp_entries_to_the_bottom() {
    // Mix a timestamped entry with a default-empty-timestamp entry. The
    // empty entry must rank LAST (sessions never opened are least relevant).
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "never-opened".to_string(),
        SessionEntry {
            title_override: Some("never-opened-session".to_string()),
            // last_opened_at left as default ("")
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "opened".to_string(),
        SessionEntry {
            title_override: Some("opened-session".to_string()),
            last_opened_at: "2024-01-01T00:00:00Z".to_string(),
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["opened-session", "never-opened-session"],
        "entries with empty last_opened_at must sort to the end"
    );
}
