//! Contract tests for IssueBrowserView key handling and event absorption.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::{IssueDetailEvent, IssueSummaryEvent, SpurEvent, SpurEventBody};
use spur_core::ExecutorLineage;
use spur_tui::action::{Action, IssueAction, ViewId};
use spur_tui::views::issue_browser::IssueBrowserView;
use spur_tui::views::{View, ViewContext};

fn test_ctx() -> ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<ExecutorLineage> =
        std::sync::LazyLock::new(ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn rendered_buffer_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut rendered = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            rendered.push_str(buf[(x, y)].symbol());
        }
        rendered.push('\n');
    }
    rendered
}

fn sample_summary(id: &str, title: &str, status: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        id: id.into(),
        source: "github".into(),
        title: title.into(),
        status: status.into(),
        priority: None,
        issue_type: None,
        assignee: None,
    }
}

fn sample_pm_summary(id: &str, title: &str) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: id.into(),
        source: spur_pm::PmSource::Beads,
        title: title.into(),
        status: "open".into(),
        labels: Vec::new(),
        url: String::new(),
        priority: Some(1),
        issue_type: Some("bug".into()),
        assignee: None,
    }
}

fn seed_issues(view: &mut IssueBrowserView) {
    let event = SpurEvent::now(SpurEventBody::IssuesLoaded {
        issues: vec![
            sample_summary("issue-1", "First issue", "open"),
            sample_summary("issue-2", "Second issue", "blocked"),
        ],
    });
    view.handle_spur_event(&event, &test_ctx());
}

fn sample_detail_event(id: &str) -> IssueDetailEvent {
    let now = Utc::now();
    IssueDetailEvent {
        id: id.into(),
        source: "github".into(),
        title: "First issue".into(),
        body: "Description".into(),
        status: "open".into(),
        labels: vec![],
        assignee: None,
        url: "".into(),
        priority: Some(1),
        issue_type: Some("bug".into()),
        blocked_by: vec![],
        due_at: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn seed_issues_renders_seeded_rows() {
    let mut view = IssueBrowserView::new();
    view.seed_issues(vec![sample_pm_summary(
        "bd-1809",
        "IssueBrowser starts populated",
    )]);

    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let ctx = test_ctx();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();

    let rendered = rendered_buffer_text(&terminal);
    assert!(
        rendered.contains("bd-1809"),
        "seeded issue id should render, rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("IssueBrowser starts populated"),
        "seeded issue title should render, rendered:\n{rendered}"
    );
}

#[test]
fn empty_browser_esc_navigates_back() {
    let mut view = IssueBrowserView::default();
    let action = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(
        matches!(action, Some(Action::NavigateTo(ViewId::Dashboard))),
        "expected NavigateTo(Dashboard), got {:?}",
        action
    );
}

#[test]
fn j_moves_selection_down() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);

    let action = view.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectNextBy(1))),
        "expected SelectNextBy(1), got {:?}",
        action
    );
}

#[test]
fn k_moves_selection_up() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);
    // Move down first so we can move up
    view.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );

    let action = view.handle_key(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectPrevBy(1))),
        "expected SelectPrevBy(1), got {:?}",
        action
    );
}

#[test]
fn enter_on_selected_issue_requests_detail() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);

    let action = view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(
            action,
            Some(Action::Issue(IssueAction::ViewDetail { ref id })) if id == "issue-1"
        ),
        "expected ViewDetail(issue-1), got {:?}",
        action
    );
}

#[test]
fn enter_while_loaded_closes_detail() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);
    view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    // Simulate detail arriving
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-1".into(),
            issue: sample_detail_event("issue-1"),
        }),
        &test_ctx(),
    );
    assert!(view.issue_detail_visible());

    // Enter again closes detail
    let action = view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(action.is_none());
    assert!(!view.issue_detail_visible());
}

#[test]
fn status_keys_emit_update_actions() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);

    let cases: Vec<(char, &str, bool)> = vec![
        ('o', "open", false),
        ('w', "in_progress", false),
        ('b', "blocked", false),
        ('x', "closed", false),
        ('d', "closed", true),
    ];

    for (key, expected_status, expected_legacy) in cases {
        let action = view.handle_key(
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
            &test_ctx(),
        );
        assert!(
            matches!(
                action,
                Some(Action::Issue(IssueAction::UpdateStatus {
                    ref id,
                    ref status,
                    via_legacy_key,
                }))
                if id == "issue-1"
                    && status == expected_status
                    && via_legacy_key == expected_legacy
            ),
            "key '{}' should emit UpdateStatus({}, {}), got {:?}",
            key,
            "issue-1",
            expected_status,
            action
        );
    }
}

#[test]
fn w_key_emits_work_on_action() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);

    let action = view.handle_key(
        KeyEvent::new(KeyCode::Char('W'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(
            action,
            Some(Action::Issue(IssueAction::WorkOn { ref id })) if id == "issue-1"
        ),
        "expected WorkOn(issue-1), got {:?}",
        action
    );
}

#[test]
fn esc_while_loaded_closes_detail_not_navigates() {
    let mut view = IssueBrowserView::default();
    seed_issues(&mut view);
    view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-1".into(),
            issue: sample_detail_event("issue-1"),
        }),
        &test_ctx(),
    );

    let action = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(action.is_none());
    assert!(!view.issue_detail_visible());
}

#[test]
fn s_key_opens_session_picker() {
    let mut view = IssueBrowserView::default();
    let action = view.handle_key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::RequestSessions)),
        "expected RequestSessions, got {:?}",
        action
    );
}
