//! Defensive selection: `auto_approve` must prefer an allow-class option
//! even when it is not the first entry in the list. Exercised via a
//! `#[doc(hidden)] pub` re-export from spur_acp::connection::native.

use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SelectedPermissionOutcome, ToolCallUpdate, ToolCallUpdateFields,
};

fn mk_request(options: Vec<PermissionOption>) -> RequestPermissionRequest {
    let tool_call = ToolCallUpdate::new("t", ToolCallUpdateFields::new());
    RequestPermissionRequest::new("s", tool_call, options)
}

#[test]
fn auto_approve_prefers_allow_always_when_not_first() {
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
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow_always");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_prefers_allow_once_when_no_allow_always() {
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
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow_once");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_falls_back_to_first_when_no_allow_kind() {
    // Degenerate case: only reject-class options. Preserves the historical
    // "options.first()" fallback — caller sees an auto-reject, which is
    // still defensible as a fail-safe.
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
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "reject_once");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_empty_options_uses_allow_default() {
    let req = mk_request(vec![]);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}
