use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::session_metadata::{SessionEntry, SessionMetadataStore};
use tempfile::tempdir;

#[test]
fn load_missing_file_returns_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let store = SessionMetadataStore::load(&path);
    assert!(store.metadata().sessions.is_empty());
    assert!(store.metadata().last_active_session_id.is_none());
}

#[test]
fn save_then_load_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");

    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry(
        "abc123".to_string(),
        SessionEntry {
            title_override: Some("My session".into()),
            last_opened_at: "2026-04-13T18:40:15Z".into(),
            draft: "hello world".into(),
            pinned: true,
            archived: false,
            ..Default::default()
        },
    );
    store.set_last_active("abc123".to_string(), "2026-04-13T18:42:00Z".into());
    store.save().unwrap();

    let store2 = SessionMetadataStore::load(&path);
    let entry = store2.metadata().sessions.get("abc123").unwrap();
    assert_eq!(entry.title_override.as_deref(), Some("My session"));
    assert_eq!(entry.draft, "hello world");
    assert!(entry.pinned);
    assert_eq!(
        store2.metadata().last_active_session_id.as_deref(),
        Some("abc123")
    );
}

#[test]
fn save_is_atomic_via_tmp_rename() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry("x".into(), SessionEntry::default());
    store.save().unwrap();
    assert!(!path.with_extension("json.tmp").exists());
    assert!(path.exists());
}

#[test]
fn load_malformed_file_returns_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(&path, "{not json").unwrap();
    let store = SessionMetadataStore::load(&path);
    assert!(store.metadata().sessions.is_empty());
}

#[test]
fn load_legacy_string_input_history_upgrades_to_structured_entries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(
        &path,
        r#"{"version":1,"sessions":{},"input_history":["look at @src/foo.rs"]}"#,
    )
    .unwrap();

    let store = SessionMetadataStore::load(&path);
    assert_eq!(store.metadata().input_history.len(), 1);
    assert_eq!(
        store.metadata().input_history[0].snapshot.text,
        "look at @src/foo.rs"
    );
    assert!(store.metadata().input_history[0]
        .snapshot
        .protected_ranges
        .is_empty());
}

#[test]
fn save_then_load_roundtrip_structured_input_history() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");

    let mut store = SessionMetadataStore::load(&path);
    store.metadata_mut().input_history = vec![InputHistoryEntry::new(InputStateSnapshot::new(
        "check @src/foo.rs".into(),
        vec![ProtectedRange {
            start: 6,
            end: 17,
            uri: "file:///abs/src/foo.rs".into(),
            name: "src/foo.rs".into(),
        }],
    ))];
    store.save().unwrap();

    let store2 = SessionMetadataStore::load(&path);
    assert_eq!(store2.metadata().input_history.len(), 1);
    let restored = &store2.metadata().input_history[0].snapshot;
    assert_eq!(restored.text, "check @src/foo.rs");
    assert_eq!(restored.protected_ranges.len(), 1);
    assert_eq!(restored.protected_ranges[0].uri, "file:///abs/src/foo.rs");
}

#[test]
fn gc_removes_entries_for_sessions_not_in_live_list() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);

    for id in ["alive1", "alive2", "gone1", "gone2"] {
        store.upsert_entry(id.to_string(), SessionEntry::default());
    }

    let live_ids: Vec<String> = vec!["alive1".into(), "alive2".into()];
    let removed = store.gc_orphans(&live_ids);
    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&"gone1".to_string()));
    assert!(removed.contains(&"gone2".to_string()));
    assert_eq!(store.metadata().sessions.len(), 2);
}

#[test]
fn gc_clears_last_active_when_that_session_is_orphaned() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry("gone".into(), SessionEntry::default());
    store.set_last_active("gone".into(), "2026-04-13T00:00:00Z".into());
    store.gc_orphans(&[]);
    assert!(store.metadata().last_active_session_id.is_none());
}

#[test]
fn clear_last_active_full_nulls_all_auto_resume_pointers() {
    // After BrainRetired, the TUI must null every `last_active_*` field
    // — not just `last_active_session_id`. Otherwise spur-cli's
    // `last_active_acp()` still returns (acp_id, brain) for the retired
    // session and auto-resumes it on the next launch, contradicting the
    // user's /clear intent.
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);

    store.set_acp_mapping("spur-1", "acp-abc", "claude-code-acp");
    store.set_last_active("spur-1".into(), "2026-04-20T00:00:00Z".into());
    assert!(store.last_active_acp().is_some(), "precondition");

    store.clear_last_active_full();

    assert!(store.metadata().last_active_session_id.is_none());
    assert!(store.metadata().last_active_at.is_none());
    assert!(store.metadata().last_active_acp_session_id.is_none());
    assert!(store.metadata().last_active_brain.is_none());
    assert!(
        store.last_active_acp().is_none(),
        "spur-cli must see no auto-resume target after full clear"
    );
}
