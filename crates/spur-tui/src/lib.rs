pub mod action;
pub mod agents;
pub mod app;
pub mod commands;
pub mod components;
pub mod input_history;
pub mod mentions;
pub mod session_metadata;
pub mod tui;
pub mod views;
pub mod worker_streams;

pub use app::{run_tui, UserInput};

#[doc(hidden)]
pub mod test_support {
    //! Test-only helpers that poke at session state without spinning up a
    //! full `App`. Not a stable API; gated behind a hidden module so
    //! integration tests in `tests/` can exercise `apply_session_update`.

    use crate::views::session_detail::SessionDetailView;
    use spur_acp::{SessionId, SessionNotification, SpurEvent};

    /// Build a `ViewContext` backed by the given lineage and an idle brain
    /// status. Suitable for integration tests that don't exercise brain
    /// status rendering.
    pub fn test_view_ctx(
        lineage: &spur_core::lineage::projection::ExecutorLineage,
    ) -> crate::views::ViewContext<'_> {
        static IDLE: crate::app::BrainStatus = crate::app::BrainStatus::Idle;
        crate::views::ViewContext {
            lineage,
            brain_status: &IDLE,
            license_badge: None,
        }
    }

    /// Build a minimal `Arc<AgentConfig>` with all-default nested blocks,
    /// suitable for constructing `SessionDetailView` in integration tests
    /// that don't exercise any ingest/response bindings.
    pub fn default_agent_config(name: &str) -> std::sync::Arc<spur_acp::AgentConfig> {
        std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(name))
    }

    /// Build a fresh `SessionDetailView` with placeholder identity fields so
    /// tests can apply session updates to it.
    pub fn new_session_state() -> SessionDetailView {
        SessionDetailView::new(
            SessionId("test".to_string()),
            "test-agent".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("."),
            default_agent_config("test-agent"),
            Vec::new(),
        )
    }

    /// Route a `SessionNotification`'s update through
    /// `app::apply_session_update` for assertions in tests.
    pub fn apply_notification(state: &mut SessionDetailView, notif: &SessionNotification) {
        crate::app::apply_session_update(state, &notif.update);
    }

    /// Build a fresh `App` with no user-input channel (actions emitted by
    /// `App::process_action` will silently drop). Intended for assertions
    /// that don't exercise the outbound channel.
    pub fn new_app() -> crate::app::App {
        crate::app::App::new(None, false)
    }

    /// Dispatch a `SpurEvent` into the app exactly as the runtime loop would.
    pub fn push_event(app: &mut crate::app::App, ev: SpurEvent) {
        app.handle_spur_event(ev);
    }

    /// Borrow the current `SessionDetailView`, if one exists.
    pub fn session_detail(app: &crate::app::App) -> Option<&SessionDetailView> {
        app.session_detail_for_test()
    }
}
