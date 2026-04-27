use spur_tui::components::input_bar::{ProtectedRange, RangeKind};
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::session_metadata::{
    current_metadata_version, SessionEntry, SessionMetadata, SessionMetadataStore,
};
use tempfile::tempdir;

#[test]
fn load_missing_file_returns_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let store = SessionMetadataStore::load(&path);
    assert_eq!(
        store.metadata().schema_version,
        current_metadata_version(),
        "fresh-install store must use the current schema version"
    );
    assert!(store.metadata().sessions.is_empty());
    assert!(store.metadata().last_active_session_id.is_none());
}

#[test]
fn default_metadata_uses_current_schema_version() {
    assert_eq!(
        SessionMetadata::default().schema_version,
        current_metadata_version(),
        "in-memory defaults must match serde's on-disk default"
    );
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
            kind: RangeKind::Atom,
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
fn one_bad_input_history_entry_drops_only_that_entry() {
    // Catastrophic-data-loss regression guard: a single non-conforming entry
    // in the persisted JSON array must NOT void the rest of the history.
    // Prior to per-entry tolerant deserialize, one bad entry returned an
    // error from the field-level deserializer, the whole `from_str` call
    // failed, and `load()` fell back to an empty store -> 100 entries -> 0.
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(
        &path,
        r#"{"version":1,"sessions":{},"input_history":["good 1", 42, "good 2"]}"#,
    )
    .unwrap();

    let store = SessionMetadataStore::load(&path);
    let history = &store.metadata().input_history;
    assert_eq!(history.len(), 2, "two valid entries must survive");
    assert_eq!(history[0].snapshot.text, "good 1");
    assert_eq!(history[1].snapshot.text, "good 2");
}

#[test]
fn one_bad_protected_range_drops_only_that_range() {
    // A single malformed `protected_range` inside an entry's snapshot must
    // not void the entry. The bad range is dropped, the entry survives,
    // and `sanitized()` sees the surviving valid ranges.
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(
        &path,
        r#"{
            "version": 1,
            "sessions": {},
            "input_history": [
                {
                    "text": "@a tail",
                    "protected_ranges": [
                        {"start": 0, "end": 2, "uri": "u1", "name": "a"},
                        {"uri": "missing-fields-only"}
                    ]
                }
            ]
        }"#,
    )
    .unwrap();

    let store = SessionMetadataStore::load(&path);
    let history = &store.metadata().input_history;
    assert_eq!(history.len(), 1, "entry must survive a bad range");
    let snapshot = &history[0].snapshot;
    assert_eq!(snapshot.text, "@a tail");
    assert_eq!(
        snapshot.protected_ranges.len(),
        1,
        "the valid range survives, the malformed range is dropped"
    );
    assert_eq!(snapshot.protected_ranges[0].uri, "u1");
}

#[test]
fn all_bad_input_history_entries_yield_empty_vec() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(
        &path,
        r#"{"version":1,"sessions":{},"input_history":[42, true, null]}"#,
    )
    .unwrap();

    let store = SessionMetadataStore::load(&path);
    assert!(
        store.metadata().input_history.is_empty(),
        "all-bad list yields empty Vec, not a load failure"
    );
    // The rest of the document still loads.
    assert_eq!(store.metadata().schema_version, current_metadata_version());
}

#[test]
fn old_schema_version_loads_tolerantly_and_normalizes_to_current() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(
        &path,
        r#"{"version":0,"sessions":{},"input_history":["hello"]}"#,
    )
    .unwrap();

    let store = SessionMetadataStore::load(&path);
    assert_eq!(store.metadata().input_history.len(), 1);
    assert_eq!(store.metadata().input_history[0].snapshot.text, "hello");
    assert_eq!(
        store.metadata().schema_version,
        current_metadata_version(),
        "old schema versions must be normalized to CURRENT in memory"
    );
}

#[test]
fn unknown_future_schema_version_refuses_save_and_preserves_unknown_fields() {
    // A future version (> CURRENT) must not drop the document, but this
    // binary cannot preserve unknown fields when serializing typed structs.
    // Saving therefore no-ops so forward-compat data remains on disk.
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let future = current_metadata_version() + 99;
    let written = format!(
        r#"{{"version":{future},"extra_field":"preserved_data","sessions":{{}},"input_history":["hello"]}}"#,
    );
    std::fs::write(&path, written).unwrap();

    let store = SessionMetadataStore::load(&path);
    assert_eq!(store.metadata().input_history.len(), 1);
    assert_eq!(store.metadata().input_history[0].snapshot.text, "hello");
    assert_eq!(
        store.metadata().schema_version,
        current_metadata_version(),
        "in-memory schema_version is reset to CURRENT after a future-version load"
    );

    store.save().unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        value["version"].as_u64().unwrap(),
        u64::from(future),
        "future-version files must not be downgraded on disk"
    );
    assert_eq!(
        value["extra_field"].as_str(),
        Some("preserved_data"),
        "unknown fields must survive a refused save"
    );
}

#[test]
fn valid_data_roundtrip_unchanged_by_tolerant_deserialize() {
    // Behaviour-preservation guard: when nothing is malformed, the
    // tolerant per-entry / per-range path must produce identical results
    // to the prior strict path.
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");

    let original = vec![
        InputHistoryEntry::new(InputStateSnapshot::new("plain text".into(), Vec::new())),
        InputHistoryEntry::new(InputStateSnapshot::new(
            "@x tail".into(),
            vec![ProtectedRange {
                start: 0,
                end: 2,
                kind: RangeKind::Atom,
                uri: "uri-x".into(),
                name: "x".into(),
            }],
        )),
    ];

    let mut store = SessionMetadataStore::load(&path);
    store.metadata_mut().input_history = original.clone();
    store.save().unwrap();

    let store2 = SessionMetadataStore::load(&path);
    assert_eq!(store2.metadata().input_history, original);
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
