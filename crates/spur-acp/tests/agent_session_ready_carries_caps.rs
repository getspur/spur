use std::sync::Arc;

use agent_client_protocol::schema::{InitializeResponse, NewSessionResponse, ProtocolVersion};
use spur_acp::{AgentKind, CancelMode, SessionId, SpurAgentCaps, SpurEvent, SpurEventBody};

#[test]
fn agent_session_ready_serializes_caps_slot() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let new = NewSessionResponse::new(agent_client_protocol::schema::SessionId::new("acp-session"));
    let caps = Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp));

    let event = SpurEvent::now(SpurEventBody::AgentSessionReady {
        session: SessionId("spur-session".to_string()),
        acp_session_id: "acp-session".to_string(),
        brain: "codex".to_string(),
        resumed: false,
        cancel_mode: CancelMode::AcpSoft,
        fs_unsafe: false,
        caps: Some(caps),
    });

    let value = serde_json::to_value(&event).expect("event must serialize");
    let ready = value
        .get("body")
        .and_then(|body| body.get("AgentSessionReady"))
        .expect("serialized body must be AgentSessionReady");

    assert!(
        ready.get("caps").is_some(),
        "AgentSessionReady must carry a caps slot; serialized event: {ready}"
    );
    assert!(
        ready.get("caps").is_some_and(serde_json::Value::is_object),
        "caps must serialize as an object when populated; serialized event: {ready}"
    );

    let decoded: SpurEvent = serde_json::from_value(value).expect("event must round-trip");
    match decoded.body {
        SpurEventBody::AgentSessionReady {
            caps: Some(caps), ..
        } => {
            assert!(!caps.supports_set_model());
            assert!(!caps.supports_set_config_option());
        }
        other => panic!("expected AgentSessionReady with caps after round-trip, got {other:?}"),
    }
}
