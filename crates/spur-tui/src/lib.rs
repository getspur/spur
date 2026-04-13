pub mod action;
pub mod app;
pub mod commands;
pub mod components;
pub mod mentions;
pub mod tui;
pub mod views;

pub use app::{run_tui, UserInput};

#[doc(hidden)]
pub mod test_support {
    //! Test-only helpers that poke at session state without spinning up a
    //! full `App`. Not a stable API; gated behind a hidden module so
    //! integration tests in `tests/` can exercise `apply_session_update`.

    use crate::views::session_detail::SessionDetailView;
    use spur_acp::{SessionId, SessionNotification};

    /// Build a fresh `SessionDetailView` with placeholder identity fields so
    /// tests can apply session updates to it.
    pub fn new_session_state() -> SessionDetailView {
        SessionDetailView::new(
            SessionId("test".to_string()),
            "test-agent".to_string(),
            "brain".to_string(),
        )
    }

    /// Route a `SessionNotification`'s update through
    /// `app::apply_session_update` for assertions in tests.
    pub fn apply_notification(state: &mut SessionDetailView, notif: &SessionNotification) {
        crate::app::apply_session_update(state, &notif.update);
    }
}
