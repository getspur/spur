pub mod action;
pub mod agents;
pub mod app;
pub mod commands;
pub mod components;
pub mod input_history;
pub mod landing;
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

    use crate::{action::Action, views::session_detail::SessionDetailView};
    use spur_acp::{SessionId, SessionNotification, SpurEvent};

    /// Build a `ViewContext` backed by the given lineage and an idle brain
    /// status. Suitable for integration tests that don't exercise brain
    /// status rendering.
    pub fn test_view_ctx(
        lineage: &spur_core::lineage::projection::ExecutorLineage,
    ) -> crate::views::ViewContext<'_> {
        static IDLE: crate::app::BrainStatus = crate::app::BrainStatus::Idle;
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &IDLE,
            license_badge: None,
            flag_summary: None,
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

    /// Build a fresh `App` with a live user-input channel for UAT tests.
    pub fn app_with_user_input_tx() -> (
        crate::app::App,
        tokio::sync::mpsc::Receiver<crate::app::UserInput>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::app::UserInput>(64);
        (crate::app::App::new(Some(tx), false), rx)
    }

    /// Dispatch a `SpurEvent` into the app exactly as the runtime loop would.
    pub fn push_event(app: &mut crate::app::App, ev: SpurEvent) {
        app.handle_spur_event(ev);
    }

    /// Dispatch an `Action` through the app controller.
    pub fn process_action(app: &mut crate::app::App, action: Action) {
        app.process_action(action);
    }

    /// Borrow the current `SessionDetailView`, if one exists.
    pub fn session_detail(app: &crate::app::App) -> Option<&SessionDetailView> {
        app.session_detail_for_test()
    }

    /// Borrow the pending first user message, if one exists.
    pub fn pending_first_user_message(app: &crate::app::App) -> Option<&str> {
        app.pending_first_user_message_for_test()
    }
}
