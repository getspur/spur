use spur_bot::state::{BotStateStore, PersistedBotState};

#[test]
fn persisted_state_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let store = BotStateStore::new(path.clone());

    let expected = PersistedBotState {
        version: 1,
        operator_chat_id: Some(10_001),
        current_acp_session_id: Some("acp_77".into()),
        current_brain: Some("claude-code".into()),
    };

    store.save(&expected).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, expected);
}

#[test]
fn missing_state_file_defaults_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join("missing.json"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded.current_acp_session_id, None);
    assert_eq!(loaded.current_brain, None);
}
