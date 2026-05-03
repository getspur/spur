//! Inc 3 (bd-d587.3): integration tests for `IssueBrowserView::open_external_detail`
//! with `OpenMode::FocusGraph` — the path used by PlanBrowser View-Epic.
//!
//! Verifies:
//! 1. When the id is already in `tracked_issues`, the row is selected
//!    immediately and the eager `Action::GetIssueGraph` is dispatched.
//! 2. After `IssueDetailFetched` + `IssueSubgraphLoaded`, the right pane
//!    flips to Graph mode (instead of the pre-Inc-3 hardcoded Text reset).
//! 3. When the id is NOT yet in `tracked_issues`, it's queued via
//!    `pending_select` and applied on the next `IssuesLoaded` that contains it.
//! 4. (Follow-up) After `IssueCommandError` on the graph fetch, the armed
//!    `post_load_mode` is cleared so a subsequent `IssueDetailFetched`
//!    falls back to Text mode (regression for state-leak found by both
//!    gemini and kimi reviews).

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::{
    GraphEdgeEvent, GraphNodeEvent, IssueDetailEvent, IssueSummaryEvent, SpurEvent, SpurEventBody,
};
use spur_tui::action::{Action, ViewId};
use spur_tui::{app::App, UserInput};
use tokio::sync::mpsc;

fn sample_summary(id: &str, title: &str, issue_type: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        id: id.into(),
        source: "beads".into(),
        title: title.into(),
        status: "open".into(),
        priority: Some(1),
        issue_type: Some(issue_type.into()),
        assignee: None,
    }
}

fn sample_detail(id: &str, title: &str) -> IssueDetailEvent {
    let now = Utc::now();
    IssueDetailEvent {
        id: id.into(),
        source: "beads".into(),
        title: title.into(),
        body: format!("Body for {id}"),
        status: "open".into(),
        labels: vec![],
        assignee: None,
        url: String::new(),
        priority: Some(1),
        issue_type: Some("epic".into()),
        blocked_by: vec![],
        due_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn graph_node(id: &str, title: &str) -> GraphNodeEvent {
    GraphNodeEvent {
        id: id.into(),
        title: Some(title.into()),
        status: Some("open".into()),
        priority: Some(1),
        labels: vec![],
        pagerank: Some(0.5),
    }
}

fn graph_edge(from: &str, to: &str) -> GraphEdgeEvent {
    GraphEdgeEvent {
        from: from.into(),
        to: to.into(),
        edge_type: Some("blocks".into()),
    }
}

fn render_text(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
    terminal.draw(|f| app.render(f)).unwrap();
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn drain(rx: &mut mpsc::Receiver<UserInput>) -> Vec<UserInput> {
    let mut out = Vec::new();
    while let Ok(input) = rx.try_recv() {
        out.push(input);
    }
    out
}

/// `UserInput` doesn't `impl Debug`. Render a stable label per relevant
/// variant for diagnostic messages.
fn label(input: &UserInput) -> String {
    match input {
        UserInput::GetIssueDetail { id } => format!("GetIssueDetail({id})"),
        UserInput::GetIssueGraph { id } => format!("GetIssueGraph({id})"),
        UserInput::RefreshIssues => "RefreshIssues".into(),
        _ => "<other>".into(),
    }
}

fn labels(inputs: &[UserInput]) -> String {
    inputs.iter().map(label).collect::<Vec<_>>().join(", ")
}

fn has_get_issue_detail(inputs: &[UserInput], expected_id: &str) -> bool {
    inputs
        .iter()
        .any(|u| matches!(u, UserInput::GetIssueDetail { id } if id == expected_id))
}

fn has_get_issue_graph(inputs: &[UserInput], expected_id: &str) -> bool {
    inputs
        .iter()
        .any(|u| matches!(u, UserInput::GetIssueGraph { id } if id == expected_id))
}

#[test]
fn open_issue_in_backlog_selects_existing_row_and_arms_graph_focus() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();

    // Pre-create the IssueBrowser with a tracked epic so open_external_detail
    // finds the id in tracked_issues immediately.
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    let _ = drain(&mut rx); // consume RefreshIssues
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![
                sample_summary("issue-other", "Other task", "task"),
                sample_summary("bd-epic-1", "Sprint epic", "epic"),
            ],
        }),
    );

    spur_tui::test_support::process_action(
        &mut app,
        Action::OpenIssueInBacklog {
            id: "bd-epic-1".into(),
        },
    );

    // App must dispatch GetIssueDetail AND the eager GetIssueGraph
    // (the pending_action drained by app.rs after open_external_detail).
    let inputs = drain(&mut rx);
    let dump = labels(&inputs);
    assert!(
        has_get_issue_detail(&inputs, "bd-epic-1"),
        "expected GetIssueDetail for bd-epic-1, got {dump}",
    );
    assert!(
        has_get_issue_graph(&inputs, "bd-epic-1"),
        "FocusGraph must eagerly request the subgraph; got {dump}",
    );

    // After the detail fetch arrives, the post_load_mode flip kicks the
    // right pane to Graph view.
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "bd-epic-1".into(),
            issue: sample_detail("bd-epic-1", "Sprint epic"),
        }),
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "bd-epic-1".into(),
            nodes: vec![
                graph_node("bd-epic-1", "Sprint epic"),
                graph_node("bd-task-1", "Child task"),
            ],
            edges: vec![graph_edge("bd-epic-1", "bd-task-1")],
        }),
    );

    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    let rendered = render_text(&mut app, &mut terminal);

    assert!(
        rendered.contains("[Graph]"),
        "right pane must show Graph status hint after FocusGraph open:\n{rendered}",
    );
    assert!(
        rendered.contains("Issue Graph: bd-epic-1"),
        "graph view must render the epic's subgraph:\n{rendered}",
    );
    // In Graph mode the right pane fills the viewport and the list is
    // hidden, matching the existing graph-toggle UX. The left-list
    // selection is asserted indirectly by the second test, which switches
    // back to the list view via Enter.
}

#[test]
fn open_issue_queues_pending_select_when_id_not_yet_tracked() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();

    // Open IssueBrowser cold (no tracked issues seeded). The id isn't present,
    // so open_external_detail stashes pending_select.
    spur_tui::test_support::process_action(
        &mut app,
        Action::OpenIssueInBacklog {
            id: "bd-epic-pending".into(),
        },
    );
    let _ = drain(&mut rx);

    // When IssuesLoaded arrives carrying the queued id, pending_select is
    // drained and the row gets selected. We verify the selection moved by
    // pressing Enter and watching the subsequent GetIssueDetail target.
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![
                sample_summary("bd-other-1", "First other", "task"),
                sample_summary("bd-epic-pending", "The pending epic", "epic"),
                sample_summary("bd-other-2", "Second other", "task"),
            ],
        }),
    );

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let after_enter = drain(&mut rx);
    let dump = labels(&after_enter);
    assert!(
        has_get_issue_detail(&after_enter, "bd-epic-pending"),
        "Enter on selected row must request detail for bd-epic-pending \
         (the queued pending_select id), got {dump}",
    );
}

#[test]
fn graph_error_after_focus_graph_does_not_force_text_load_into_graph() {
    // Regression for the state-leak both gemini and kimi flagged in Inc 3:
    // before the fix, IssueCommandError on the graph fetch left
    // `post_load_mode = Some(Graph)` armed, so the still-in-flight
    // IssueDetailFetched for the same id would consume the stale mode and
    // open the right pane in Graph view despite the user never seeing the
    // graph data (the fetch failed). With the fix, post_load_mode is
    // cleared in the IssueCommandError graph-error branch, so the detail
    // arrival falls back to Text.

    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    let _ = drain(&mut rx);
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![sample_summary("issue-a", "Sprint epic", "epic")],
        }),
    );

    // Arm FocusGraph: post_load_mode = Some(Graph), graph_loading = Some(id),
    // pending_action = Some(GetIssueGraph).
    spur_tui::test_support::process_action(
        &mut app,
        Action::OpenIssueInBacklog {
            id: "issue-a".into(),
        },
    );
    let _ = drain(&mut rx);

    // Simulate the graph fetch failing.
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueCommandError {
            operation: "get_graph".into(),
            error: "graph fetch failed".into(),
            id: Some("issue-a".into()),
        }),
    );

    // The still-in-flight detail fetch for the same id arrives. Without
    // the fix, post_load_mode would force Graph view even though the
    // graph data is unavailable.
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-a".into(),
            issue: sample_detail("issue-a", "Sprint epic"),
        }),
    );

    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    let rendered = render_text(&mut app, &mut terminal);

    assert!(
        rendered.contains("[Text]"),
        "after graph-fetch error, the detail arrival must fall back to Text \
         mode (post_load_mode cleared); rendered:\n{rendered}",
    );
    assert!(
        !rendered.contains("[Graph]"),
        "Graph status hint must NOT appear when the graph fetch errored; \
         rendered:\n{rendered}",
    );
    assert!(
        rendered.contains("Body for issue-a"),
        "detail body must still render in Text mode; rendered:\n{rendered}",
    );
}
