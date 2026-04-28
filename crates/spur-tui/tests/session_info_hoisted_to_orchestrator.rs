//! M9 F-3-1 — `SessionInfoCache` lives on the orchestrator entry, not
//! the transient `SessionDetailView`.
//!
//! Source: `docs/superpowers/plans/2026-04-27-m9-spur-acp-followups.md` §1.
//!
//! Test exercises the orchestrator-side cache directly. Constructing a
//! fresh `SessionDetailView` (simulating "navigate away and back")
//! demonstrates that view lifecycle does not touch the orchestrator's
//! cache — the cached title survives the view rebuild because the data
//! never lived on the view to begin with.

use agent_client_protocol::schema::SessionInfoUpdate;
use spur_acp::{SessionId, SpurConfig, TestStubConnection};
use spur_core::orchestrator::{BrainSession, Orchestrator};
use spur_tui::test_support::default_agent_config;
use spur_tui::views::session_detail::SessionDetailView;

#[tokio::test]
async fn session_info_cache_survives_view_rebuild() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = SpurConfig::default();
    config.cost.db_path = tmp.path().join("cost.db").display().to_string();
    let orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

    let mut brain = BrainSession::for_test(
        Box::new(TestStubConnection),
        "acp-x",
        SessionId("spur-x".to_string()),
        "test-brain",
    );

    // Pre-condition: orchestrator has no cached session info yet.
    assert!(
        orchestrator.session_info(&brain).is_none(),
        "fresh brain must have no session_info cache"
    );

    // Inject a SessionInfoUpdate { title: "First" } — orchestrator owns
    // the merge.
    let payload = SessionInfoUpdate::new()
        .title("First")
        .updated_at("2026-04-27T14:00:00Z");
    orchestrator.apply_session_info_update(&mut brain, &payload);

    // Orchestrator's getter reflects the merged cache.
    let info = orchestrator
        .session_info(&brain)
        .expect("orchestrator must populate session_info after apply");
    assert_eq!(info.title.as_deref(), Some("First"));
    assert_eq!(info.updated_at.as_deref(), Some("2026-04-27T14:00:00Z"));

    // Simulate "navigate away and back": construct a fresh
    // SessionDetailView. The view's lifecycle MUST NOT touch the
    // orchestrator's cache — the cached title survives because the data
    // never lived on the view.
    let _fresh_view = SessionDetailView::new(
        SessionId("spur-x".to_string()),
        "test-agent".to_string(),
        "brain".to_string(),
        std::path::PathBuf::from("."),
        default_agent_config("test-agent"),
        Vec::new(),
    );

    let info_after_rebuild = orchestrator
        .session_info(&brain)
        .expect("session_info must persist across SessionDetailView rebuild");
    assert_eq!(
        info_after_rebuild.title.as_deref(),
        Some("First"),
        "title must survive view rebuild — cache lives on orchestrator entry"
    );

    brain.delegation_handle.abort();
}
