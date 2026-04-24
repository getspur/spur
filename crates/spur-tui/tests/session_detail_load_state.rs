use spur_acp::{SessionId, SpurEvent, SpurEventBody};
use spur_tui::views::session_detail::{LoadState, SessionDetailView};

fn ev(body: SpurEventBody) -> SpurEvent {
    SpurEvent::now(body)
}

#[test]
fn initial_load_state_is_retiring() {
    let view = SessionDetailView::for_session(SessionId("s".to_string()));
    assert!(matches!(view.load_state(), LoadState::Retiring));
}

#[test]
fn brain_connecting_for_matching_session_transitions_to_connecting() {
    let mut view = SessionDetailView::for_session(SessionId("s".to_string()));
    view.apply_spur_event(&ev(SpurEventBody::BrainConnecting {
        session: SessionId("s".to_string()),
        brain_name: "claude-code".into(),
    }));
    assert!(matches!(view.load_state(), LoadState::Connecting { .. }));
}

#[test]
fn session_loading_transitions_to_loading() {
    let mut view = SessionDetailView::for_session(SessionId("s".to_string()));
    view.apply_spur_event(&ev(SpurEventBody::SessionLoading {
        session: SessionId("s".to_string()),
    }));
    assert!(matches!(view.load_state(), LoadState::Loading));
}

#[test]
fn session_loaded_transitions_to_ready() {
    let mut view = SessionDetailView::for_session(SessionId("s".to_string()));
    view.apply_spur_event(&ev(SpurEventBody::SessionLoaded {
        session: SessionId("s".to_string()),
    }));
    assert!(matches!(view.load_state(), LoadState::Ready));
}

#[test]
fn brain_error_for_matching_session_transitions_to_failed() {
    let mut view = SessionDetailView::for_session(SessionId("s".to_string()));
    view.apply_spur_event(&ev(SpurEventBody::BrainError {
        session: SessionId("s".to_string()),
        message: "boom".into(),
    }));
    match view.load_state() {
        LoadState::Failed { message } => assert_eq!(message, "boom"),
        other => panic!("expected Failed, got {:?}", other),
    }
}

#[test]
fn brain_error_for_different_session_is_ignored() {
    let mut view = SessionDetailView::for_session(SessionId("s".to_string()));
    view.apply_spur_event(&ev(SpurEventBody::BrainError {
        session: SessionId("other".to_string()),
        message: "boom".into(),
    }));
    // Still initial state — did not transition.
    assert!(matches!(view.load_state(), LoadState::Retiring));
}
