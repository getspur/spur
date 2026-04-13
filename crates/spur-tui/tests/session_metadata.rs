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
