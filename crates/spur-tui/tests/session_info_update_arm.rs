//! Wave E.1 — failing test for the `SessionInfoUpdate` arm in
//! `app::apply_session_update`.
//!
//! Today the catch-all `_ => trace!(...)` arm silently swallows
//! `SessionUpdate::SessionInfoUpdate(payload)` notifications. Wave E.2 adds
//! an explicit arm that caches `title` and `updated_at` on the view so any
//! agent that emits these (codex 0.12 currently emits zero per the Wave 0.3
//! probe; the arm is forward-compat for other agents and future codex
//! versions) sees the data captured.
//!
//! Mirrors `crates/spur-tui/tests/config_option_update_arm.rs`.
//!
//! The fixture lives at
//! `crates/spur-acp/tests/data/codex_session_info_update_sample.json`.

use agent_client_protocol::schema::{SessionId as AcpSessionId, SessionInfoUpdate};
use spur_acp::{SessionId, SessionNotification, SessionUpdate};
use spur_tui::test_support::{apply_notification, default_agent_config};
use spur_tui::views::session_detail::SessionDetailView;

#[test]
fn session_info_update_arm_caches_title_and_updated_at() {
    let tmp = tempfile::tempdir().unwrap();
    let session = SessionId::new();
    let mut view = SessionDetailView::new(
        session.clone(),
        "codex".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        default_agent_config("codex"),
        Vec::new(),
    );

    // Pre-condition: no cached session info.
    assert!(
        view.session_info_for_test().is_none(),
        "fresh view must have no SessionInfoUpdate cache"
    );

    let payload = SessionInfoUpdate::new()
        .title("Quick math: 2+2 and 3+3")
        .updated_at("2026-04-27T14:00:00Z");
    let update = SessionUpdate::SessionInfoUpdate(payload);
    let notif = SessionNotification::new(AcpSessionId::new("test"), update);

    apply_notification(&mut view, &notif);

    let info = view
        .session_info_for_test()
        .expect("SessionInfoUpdate arm must populate the cache");
    assert_eq!(info.title.as_deref(), Some("Quick math: 2+2 and 3+3"));
    assert_eq!(info.updated_at.as_deref(), Some("2026-04-27T14:00:00Z"));
}
