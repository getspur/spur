#[test]
fn lobby_targets_omit_message_thread_id() {
    let params = spur_bot::telegram::client::build_send_text_params(42, None, "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert!(json.get("message_thread_id").is_none());
}

#[test]
fn topic_targets_include_message_thread_id() {
    let params = spur_bot::telegram::client::build_send_text_params(42, Some(77), "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert_eq!(json.get("message_thread_id").and_then(|v| v.as_i64()), Some(77));
}

#[test]
fn general_topic_id_is_omitted_outbound() {
    let params = spur_bot::telegram::client::build_send_text_params(42, Some(1), "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert!(json.get("message_thread_id").is_none());
}
