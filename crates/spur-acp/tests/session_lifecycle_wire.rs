use std::path::PathBuf;

use serde_json::json;
use spur_acp::{
    AcpSessionId as SessionId, CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest,
    DeleteSessionResponse, ResumeSessionRequest, ResumeSessionResponse,
};

#[test]
fn resume_session_request_roundtrips_with_camel_case_fields() {
    let request = ResumeSessionRequest::new(SessionId::new("sess_123"), PathBuf::from("/tmp/spur"))
        .additional_directories(vec![PathBuf::from("/tmp/spur-extra")]);

    let value = serde_json::to_value(&request).expect("resume request must serialize");
    assert_eq!(
        value,
        json!({
            "sessionId": "sess_123",
            "cwd": "/tmp/spur",
            "additionalDirectories": ["/tmp/spur-extra"]
        })
    );

    let roundtrip: ResumeSessionRequest =
        serde_json::from_value(value).expect("resume request must deserialize");
    assert_eq!(roundtrip.session_id, SessionId::new("sess_123"));
    assert_eq!(roundtrip.cwd, PathBuf::from("/tmp/spur"));
    assert_eq!(
        roundtrip.additional_directories,
        vec![PathBuf::from("/tmp/spur-extra")]
    );
    assert!(roundtrip.mcp_servers.is_empty());
    assert!(roundtrip.meta.is_none());
}

#[test]
fn resume_session_response_roundtrips_empty_payload() {
    let response = ResumeSessionResponse::new();
    let value = serde_json::to_value(&response).expect("resume response must serialize");
    assert_eq!(value, json!({}));

    let roundtrip: ResumeSessionResponse =
        serde_json::from_value(value).expect("resume response must deserialize");
    assert_eq!(roundtrip, ResumeSessionResponse::new());
}

#[test]
fn delete_session_request_roundtrips_with_session_id() {
    let request = DeleteSessionRequest::new(SessionId::new("sess_123"));

    let value = serde_json::to_value(&request).expect("delete request must serialize");
    assert_eq!(value, json!({ "sessionId": "sess_123" }));

    let roundtrip: DeleteSessionRequest =
        serde_json::from_value(value).expect("delete request must deserialize");
    assert_eq!(roundtrip.session_id, SessionId::new("sess_123"));
    assert!(roundtrip.meta.is_none());
}

#[test]
fn delete_session_response_roundtrips_empty_payload() {
    let response = DeleteSessionResponse::new();
    let value = serde_json::to_value(&response).expect("delete response must serialize");
    assert_eq!(value, json!({}));

    let roundtrip: DeleteSessionResponse =
        serde_json::from_value(value).expect("delete response must deserialize");
    assert_eq!(roundtrip, DeleteSessionResponse::new());
}

#[test]
fn close_session_request_roundtrips_with_session_id() {
    let request = CloseSessionRequest::new(SessionId::new("sess_123"));

    let value = serde_json::to_value(&request).expect("close request must serialize");
    assert_eq!(value, json!({ "sessionId": "sess_123" }));

    let roundtrip: CloseSessionRequest =
        serde_json::from_value(value).expect("close request must deserialize");
    assert_eq!(roundtrip.session_id, SessionId::new("sess_123"));
    assert!(roundtrip.meta.is_none());
}

#[test]
fn close_session_response_roundtrips_empty_payload() {
    let response = CloseSessionResponse::new();
    let value = serde_json::to_value(&response).expect("close response must serialize");
    assert_eq!(value, json!({}));

    let roundtrip: CloseSessionResponse =
        serde_json::from_value(value).expect("close response must deserialize");
    assert_eq!(roundtrip, CloseSessionResponse::new());
}
