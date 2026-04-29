use spur_bot::state::{BindingState, BotStateStore, PersistedBotState, PersistedThreadRecord};

#[tokio::test]
async fn persisted_state_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let store = BotStateStore::new(path.clone());

    let mut expected = PersistedBotState {
        operator_chat_id: Some(10_001),
        ..PersistedBotState::default()
    };
    expected.threads.insert(
        1,
        PersistedThreadRecord {
            topic_name: "Session 1".into(),
            archived: false,
            acp_session_id: Some("acp_77".into()),
            brain: Some("claude-code".into()),
            binding_state: BindingState::Unbound,
        },
    );

    store.save(&expected).await.unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, expected);
}

#[test]
fn missing_state_file_defaults_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join("missing.json"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded.operator_chat_id, None);
    assert_eq!(loaded.threads.len(), 0);
}

#[test]
fn corrupt_state_file_returns_error_with_path_and_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    std::fs::write(&path, "{not valid json").unwrap();

    let store = BotStateStore::new(path.clone());
    let err = store.load().expect_err("corrupt JSON must surface as Err");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("parsing state file"),
        "expected parse-context phrase in chain, got: {rendered}"
    );
    assert!(
        rendered.contains(&path.display().to_string()),
        "expected path {} in chain, got: {rendered}",
        path.display()
    );
}

#[test]
fn legacy_single_binding_loads_without_data_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "operator_chat_id": 42,
            "current_acp_session_id": "acp-1",
            "current_brain": "kimi"
        })
        .to_string(),
    )
    .unwrap();

    let store = BotStateStore::new(path);
    let state = store.load().unwrap();

    assert_eq!(state.operator_chat_id, Some(42));
    assert_eq!(state.next_topic_seq, 1);
    assert_eq!(state.threads.len(), 1);
    let only = state.threads.values().next().unwrap();
    assert!(only.archived);
    assert_eq!(only.acp_session_id.as_deref(), Some("acp-1"));
    assert_eq!(only.brain.as_deref(), Some("kimi"));
}

#[tokio::test]
async fn registry_round_trips_archived_and_live_threads() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    let store = BotStateStore::new(path.clone());

    let mut state = PersistedBotState {
        operator_chat_id: Some(42),
        next_topic_seq: 3,
        ..PersistedBotState::default()
    };
    state.threads.insert(
        11,
        PersistedThreadRecord {
            topic_name: "Session 1".into(),
            archived: false,
            acp_session_id: Some("acp-11".into()),
            brain: Some("kimi".into()),
            binding_state: BindingState::RestorePending {
                acp_session_id: "acp-11".into(),
                brain: "kimi".into(),
            },
        },
    );
    state.threads.insert(
        12,
        PersistedThreadRecord {
            topic_name: "Session 2".into(),
            archived: true,
            acp_session_id: Some("acp-12".into()),
            brain: Some("kimi".into()),
            binding_state: BindingState::ArchivedDetached,
        },
    );

    store.save(&state).await.unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, state);
}
