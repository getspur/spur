use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;

fn roundtrip(body: SpurEventBody) -> SpurEventBody {
    let event = SpurEvent::now(body);
    let json = serde_json::to_string(&event).expect("serialize");
    let back: SpurEvent = serde_json::from_str(&json).expect("deserialize");
    back.body
}

#[test]
fn session_retire_start_roundtrips() {
    let body = SpurEventBody::SessionRetireStart {
        from: Some(SessionId("old".to_string())),
        to: SessionId("new".to_string()),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionRetireStart { .. }
    ));
}

#[test]
fn session_retire_complete_roundtrips() {
    let body = SpurEventBody::SessionRetireComplete {
        session: SessionId("s".to_string()),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionRetireComplete { .. }
    ));
}

#[test]
fn brain_connecting_roundtrips() {
    let body = SpurEventBody::BrainConnecting {
        session: SessionId("s".to_string()),
        brain_name: "claude-code".into(),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::BrainConnecting { .. }
    ));
}

#[test]
fn session_loading_roundtrips() {
    let body = SpurEventBody::SessionLoading {
        session: SessionId("s".to_string()),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionLoading { .. }
    ));
}

#[test]
fn session_loaded_roundtrips() {
    let body = SpurEventBody::SessionLoaded {
        session: SessionId("s".to_string()),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionLoaded { .. }
    ));
}
