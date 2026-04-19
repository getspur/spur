//! Dashboard activity-log handling of `SpurEventBody::BrainRetired`.
//!
//! Commit 1 added the event variant and wired the lineage projection.
//! The dashboard's activity log is a separate consumer that previously
//! logged `BrainSpawned` without a matching termination entry on `/clear`
//! (only `SessionCompleted` was handled). This test asserts the retire
//! event produces a visible log entry.

use spur_acp::domain::events::BrainRetireReason;
use spur_acp::{SessionId, SpurEvent, SpurEventBody};
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

#[test]
fn dashboard_logs_activity_entry_on_brain_retired() {
    let mut dash = DashboardView::new();

    // Seed a brain spawn so the log has a paired "spawned" entry.
    dash.handle_spur_event(
        &SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }),
        &test_ctx(),
    );
    assert_eq!(dash.activity_log().entries().len(), 1);

    dash.handle_spur_event(
        &SpurEvent::now(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::UserClear,
        }),
        &test_ctx(),
    );

    let entries = dash.activity_log().entries();
    assert_eq!(
        entries.len(),
        2,
        "BrainRetired must push a log entry so the spawn is not dangling"
    );
    let last = &entries[1];
    assert!(
        last.message.to_lowercase().contains("retired"),
        "retirement entry must say 'retired', got: {:?}",
        last.message
    );
}
