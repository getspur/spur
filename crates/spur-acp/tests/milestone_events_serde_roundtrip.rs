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
    let original = SpurEventBody::SessionRetireStart {
        from: Some(SessionId("old".to_string())),
        to: SessionId("new".to_string()),
    };
    let SpurEventBody::SessionRetireStart { from, to } = roundtrip(original) else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(from, Some(SessionId("old".to_string())));
    assert_eq!(to, SessionId("new".to_string()));
}

#[test]
fn session_retire_start_roundtrips_with_no_prior_session() {
    let original = SpurEventBody::SessionRetireStart {
        from: None,
        to: SessionId("fresh".to_string()),
    };
    let SpurEventBody::SessionRetireStart { from, to } = roundtrip(original) else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(from, None);
    assert_eq!(to, SessionId("fresh".to_string()));
}

#[test]
fn session_retire_complete_roundtrips() {
    let original = SpurEventBody::SessionRetireComplete {
        session: SessionId("s".to_string()),
    };
    let SpurEventBody::SessionRetireComplete { session } = roundtrip(original) else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(session, SessionId("s".to_string()));
}

#[test]
fn brain_connecting_roundtrips() {
    let original = SpurEventBody::BrainConnecting {
        session: SessionId("s".to_string()),
        brain_name: "claude-code".into(),
    };
    let SpurEventBody::BrainConnecting {
        session,
        brain_name,
    } = roundtrip(original)
    else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(session, SessionId("s".to_string()));
    assert_eq!(brain_name, "claude-code");
}

#[test]
fn session_loading_roundtrips() {
    let original = SpurEventBody::SessionLoading {
        session: SessionId("s".to_string()),
    };
    let SpurEventBody::SessionLoading { session } = roundtrip(original) else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(session, SessionId("s".to_string()));
}

#[test]
fn session_loaded_roundtrips() {
    let original = SpurEventBody::SessionLoaded {
        session: SessionId("s".to_string()),
    };
    let SpurEventBody::SessionLoaded { session } = roundtrip(original) else {
        panic!("roundtrip produced wrong variant");
    };
    assert_eq!(session, SessionId("s".to_string()));
}
