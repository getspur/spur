use spur_acp::{CancelMode, SessionId, SpurEvent, SpurEventBody};

#[test]
fn agent_session_ready_serializes_caps_slot() {
    let event = SpurEvent::now(SpurEventBody::AgentSessionReady {
        session: SessionId("spur-session".to_string()),
        acp_session_id: "acp-session".to_string(),
        brain: "codex".to_string(),
        resumed: false,
        cancel_mode: CancelMode::AcpSoft,
        fs_unsafe: false,
    });

    let value = serde_json::to_value(event).expect("event must serialize");
    let ready = value
        .get("body")
        .and_then(|body| body.get("AgentSessionReady"))
        .expect("serialized body must be AgentSessionReady");

    assert!(
        ready.get("caps").is_some(),
        "AgentSessionReady must carry a caps slot; serialized event: {ready}"
    );
}
