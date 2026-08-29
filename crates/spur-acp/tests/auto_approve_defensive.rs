//! Defensive selection: without an explicit user choice, permission handling
//! never derives authorization from option order, labels, kinds, or metadata.

use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, ToolCallUpdate, ToolCallUpdateFields,
};

fn mk_request(options: Vec<PermissionOption>) -> RequestPermissionRequest {
    let tool_call = ToolCallUpdate::new("t", ToolCallUpdateFields::new());
    RequestPermissionRequest::new("s", tool_call, options)
}

#[test]
fn auto_approve_without_user_choice_cancels_mixed_options() {
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_always"),
            "Allow Always",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow Once",
            PermissionOptionKind::AllowOnce,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    assert!(matches!(resp.outcome, RequestPermissionOutcome::Cancelled));
}

#[test]
fn auto_approve_without_user_choice_does_not_infer_from_kind() {
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow Once",
            PermissionOptionKind::AllowOnce,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    assert!(matches!(resp.outcome, RequestPermissionOutcome::Cancelled));
}

#[test]
fn auto_approve_without_user_choice_cancels_all_reject_options() {
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject_always"),
            "Reject Always",
            PermissionOptionKind::RejectAlways,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    assert!(matches!(resp.outcome, RequestPermissionOutcome::Cancelled));
}

#[test]
fn auto_approve_empty_options_cancels_instead_of_inventing_an_id() {
    let req = mk_request(vec![]);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Cancelled => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }
}
