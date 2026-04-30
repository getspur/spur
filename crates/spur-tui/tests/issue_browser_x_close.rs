use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::{IssueSummaryEvent, SpurEvent, SpurEventBody};
use spur_tui::action::{Action, ViewId};
use spur_tui::{app::App, UserInput};
use tokio::sync::mpsc;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn sample_summary(id: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        id: id.into(),
        source: "github".into(),
        title: format!("issue {id}"),
        status: "open".into(),
        priority: None,
        issue_type: None,
        assignee: None,
    }
}

fn seeded_issue_browser_app() -> (App, mpsc::Receiver<UserInput>) {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    assert_eq!(*app.current_view(), ViewId::IssueBrowser);
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected first IssueBrowser navigation to request RefreshIssues"),
        Err(err) => panic!("expected RefreshIssues after first navigation, got {err}"),
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![sample_summary("issue-1")],
        }),
    );
    (app, rx)
}

fn next_update_issue(rx: &mut mpsc::Receiver<UserInput>) -> (String, spur_pm::IssueUpdate) {
    match rx.try_recv() {
        Ok(UserInput::UpdateIssue { id, update }) => (id, update),
        Ok(_) => panic!("expected UpdateIssue, got another user input"),
        Err(err) => panic!("expected UpdateIssue, got receive error: {err}"),
    }
}

#[test]
fn x_triggers_close() {
    let (mut app, mut rx) = seeded_issue_browser_app();

    app.handle_crossterm_event_for_test(key('x'));

    let (id, update) = next_update_issue(&mut rx);
    assert_eq!(id, "issue-1");
    assert_eq!(update.status.as_deref(), Some("closed"));
    assert!(app.tombstones_for_test().has(&ViewId::IssueBrowser));
    assert!(app
        .transient_hint_for_test()
        .map(|hint| hint.text.as_str())
        .is_some_and(
            |text| text.contains("Issue 'issue-1' → closed") && text.contains("press u to undo")
        ));
}

#[test]
fn d_triggers_close_with_deprecation_toast() {
    let (mut app, mut rx) = seeded_issue_browser_app();

    app.handle_crossterm_event_for_test(key('d'));

    let (id, update) = next_update_issue(&mut rx);
    assert_eq!(id, "issue-1");
    assert_eq!(update.status.as_deref(), Some("closed"));
    assert!(app.tombstones_for_test().has(&ViewId::IssueBrowser));
    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some("d → close renamed to x")
    );
}
