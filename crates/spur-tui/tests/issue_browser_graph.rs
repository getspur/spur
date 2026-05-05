use std::time::Duration;

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::{
    GraphEdgeEvent, GraphNodeEvent, IssueDetailEvent, IssueSummaryEvent, SpurEvent, SpurEventBody,
};
use spur_tui::action::{Action, ViewId};
use spur_tui::{app::App, UserInput};
use tokio::sync::mpsc;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn sample_summary(id: &str, title: &str) -> IssueSummaryEvent {
    sample_summary_with_status(id, title, "open")
}

fn sample_summary_with_status(id: &str, title: &str, status: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        id: id.into(),
        source: "beads".into(),
        title: title.into(),
        status: status.into(),
        labels: Vec::new(),
        priority: Some(1),
        issue_type: Some("task".into()),
        assignee: None,
    }
}

fn plan_epic_summary(id: &str, title: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        issue_type: Some("epic".into()),
        labels: vec!["spur:plan-complete".into(), "spur:plan-id:plan-1".into()],
        ..sample_summary(id, title)
    }
}

fn plan_task_summary(id: &str, title: &str) -> IssueSummaryEvent {
    IssueSummaryEvent {
        labels: vec![
            "spur:plan-id:plan-1".into(),
            format!("spur:plan-task-id:{id}"),
        ],
        ..sample_summary(id, title)
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
        labels: vec!["graph".into()],
        assignee: None,
        url: String::new(),
        priority: Some(1),
        issue_type: Some("task".into()),
        blocked_by: vec![],
        due_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn graph_node(id: &str, title: &str) -> GraphNodeEvent {
    graph_node_with_status(id, title, "open")
}

fn graph_node_with_status(id: &str, title: &str, status: &str) -> GraphNodeEvent {
    GraphNodeEvent {
        id: id.into(),
        title: Some(title.into()),
        status: Some(status.into()),
        priority: Some(1),
        labels: vec![],
        pagerank: Some(0.42),
    }
}

fn graph_edge(from: &str, to: &str) -> GraphEdgeEvent {
    graph_edge_with_type(from, to, "blocks")
}

fn graph_edge_with_type(from: &str, to: &str, edge_type: &str) -> GraphEdgeEvent {
    GraphEdgeEvent {
        from: from.into(),
        to: to.into(),
        edge_type: Some(edge_type.into()),
    }
}

fn seeded_issue_browser_app() -> (App, mpsc::Receiver<UserInput>, Terminal<TestBackend>) {
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
            issues: vec![sample_summary("issue-1", "First issue")],
        }),
    );
    let terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    (app, rx, terminal)
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

fn issue_panel_height(rendered: &str) -> usize {
    rendered
        .lines()
        .position(|line| line.contains("┌ Issue Detail"))
        .expect("issue detail panel should be rendered")
}

fn expect_get_detail(rx: &mut mpsc::Receiver<UserInput>, expected_id: &str) {
    match rx.try_recv() {
        Ok(UserInput::GetIssueDetail { id }) => assert_eq!(id, expected_id),
        Ok(_) => panic!("expected GetIssueDetail"),
        Err(err) => panic!("expected GetIssueDetail, got {err}"),
    }
}

fn expect_get_graph(rx: &mut mpsc::Receiver<UserInput>, expected_id: &str) {
    match rx.try_recv() {
        Ok(UserInput::GetIssueGraph { id }) => assert_eq!(id, expected_id),
        Ok(_) => panic!("expected GetIssueGraph"),
        Err(err) => panic!("expected GetIssueGraph, got {err}"),
    }
}

fn expect_get_graph_after_debounce(
    app: &mut App,
    rx: &mut mpsc::Receiver<UserInput>,
    expected_id: &str,
) {
    app.age_issue_browser_prefetch_for_test(Duration::from_millis(200));
    app.tick();
    expect_get_graph(rx, expected_id);
}

#[test]
fn issue_browser_filters_plan_artifacts_from_work_item_list() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![
                plan_epic_summary("bd-plan", "Persisted plan epic"),
                plan_task_summary("bd-plan.1", "Plan implementation task"),
                sample_summary("bd-bug", "Standalone bug"),
            ],
        }),
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    let rendered = render_text(&mut app, &mut terminal);
    assert!(rendered.contains("bd-bug"), "rendered:\n{rendered}");
    assert!(
        !rendered.contains("bd-plan"),
        "plan artifacts should be owned by PlanBrowser, not IssueBrowser:\n{rendered}"
    );
    assert!(
        rx.try_recv().is_err(),
        "single visible work item must not trigger lineage graph prefetch"
    );

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    expect_get_detail(&mut rx, "bd-bug");
}

#[test]
fn issue_list_prefetches_dependency_graph_when_browsing_selection() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![
                sample_summary("issue-1", "First issue"),
                sample_summary("issue-2", "Second issue"),
            ],
        }),
    );

    expect_get_graph_after_debounce(&mut app, &mut rx, "issue-1");

    app.handle_crossterm_event_for_test(key('j'));

    expect_get_graph_after_debounce(&mut app, &mut rx, "issue-2");
}

#[test]
fn single_issue_graph_open_requests_graph_after_detail_loads() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![sample_summary("bd-only", "Only issue")],
        }),
    );
    assert!(
        rx.try_recv().is_err(),
        "single visible work item must not prefetch before Graph mode is requested"
    );

    app.handle_crossterm_event_for_test(key('v'));
    expect_get_detail(&mut rx, "bd-only");

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "bd-only".into(),
            issue: sample_detail("bd-only", "Only issue"),
        }),
    );

    expect_get_graph(&mut rx, "bd-only");
}

#[test]
fn single_issue_loaded_detail_toggle_requests_graph() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![sample_summary("bd-only", "Only issue")],
        }),
    );
    assert!(
        rx.try_recv().is_err(),
        "single visible work item must not prefetch before Graph mode is requested"
    );

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    expect_get_detail(&mut rx, "bd-only");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "bd-only".into(),
            issue: sample_detail("bd-only", "Only issue"),
        }),
    );

    app.handle_crossterm_event_for_test(key('v'));

    expect_get_graph(&mut rx, "bd-only");
}

#[test]
fn issue_list_keeps_panel_height_when_navigating_from_parent_to_child() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }

    let mut issues = vec![
        sample_summary("bd-parent", "Parent epic"),
        sample_summary("bd-parent.1", "Child one"),
        sample_summary("bd-parent.2", "Child two"),
    ];
    for idx in 0..20 {
        issues.push(sample_summary(
            &format!("bd-other-{idx}"),
            "Unrelated issue",
        ));
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded { issues }),
    );
    let _ = rx.try_recv();
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "bd-parent".into(),
            nodes: vec![
                graph_node("bd-parent", "Parent epic"),
                graph_node("bd-parent.1", "Child one"),
                graph_node("bd-parent.2", "Child two"),
            ],
            edges: vec![
                graph_edge_with_type("bd-parent.1", "bd-parent", "related"),
                graph_edge_with_type("bd-parent.2", "bd-parent", "related"),
            ],
        }),
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();

    let parent_render = render_text(&mut app, &mut terminal);
    let parent_height = issue_panel_height(&parent_render);

    app.handle_crossterm_event_for_test(key('j'));
    expect_get_graph_after_debounce(&mut app, &mut rx, "bd-parent.1");
    let child_render = render_text(&mut app, &mut terminal);
    let child_height = issue_panel_height(&child_render);

    assert_eq!(
        parent_height, child_height,
        "issue pane height must remain stable while navigating parent/child:\nparent:\n{parent_render}\nchild:\n{child_render}"
    );
}

#[test]
fn issue_list_keeps_panel_height_when_lineage_data_arrives() {
    let (mut app, mut rx) = spur_tui::test_support::app_with_user_input_tx();
    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    match rx.try_recv() {
        Ok(UserInput::RefreshIssues) => {}
        Ok(_) => panic!("expected RefreshIssues, got different user input"),
        Err(err) => panic!("expected RefreshIssues, got {err}"),
    }

    let mut issues = vec![
        sample_summary("bd-parent", "Parent epic"),
        sample_summary("bd-parent.1", "Child one"),
    ];
    for idx in 0..20 {
        issues.push(sample_summary(
            &format!("bd-other-{idx}"),
            "Unrelated issue",
        ));
    }
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssuesLoaded { issues }),
    );
    let _ = rx.try_recv();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let flat_render = render_text(&mut app, &mut terminal);
    let flat_height = issue_panel_height(&flat_render);

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "bd-parent".into(),
            nodes: vec![
                graph_node("bd-parent", "Parent epic"),
                graph_node("bd-parent.1", "Child one"),
            ],
            edges: vec![graph_edge_with_type("bd-parent.1", "bd-parent", "related")],
        }),
    );
    let lineage_render = render_text(&mut app, &mut terminal);
    let lineage_height = issue_panel_height(&lineage_render);

    assert_eq!(
        flat_height, lineage_height,
        "lineage data must not resize the issue pane:\nflat:\n{flat_render}\nlineage:\n{lineage_render}"
    );
}

#[test]
fn graph_toggle_flow_fetches_renders_uses_cache_and_closes() {
    let (mut app, mut rx, mut terminal) = seeded_issue_browser_app();

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    expect_get_detail(&mut rx, "issue-1");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-1".into(),
            issue: sample_detail("issue-1", "First issue"),
        }),
    );

    let text = render_text(&mut app, &mut terminal);
    assert!(text.contains("Body for issue-1"), "rendered:\n{text}");
    assert!(text.contains("[Text] j/k: Nav"), "rendered:\n{text}");

    app.handle_crossterm_event_for_test(key('v'));
    expect_get_graph(&mut rx, "issue-1");
    let loading = render_text(&mut app, &mut terminal);
    assert!(
        loading.contains("Loading graph for issue-1"),
        "rendered:\n{loading}"
    );
    assert!(loading.contains("[Graph] j/k: Nav"), "rendered:\n{loading}");

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-1".into(),
            nodes: vec![
                graph_node("issue-1", "First issue"),
                graph_node("issue-2", "Second issue"),
            ],
            edges: vec![graph_edge("issue-1", "issue-2")],
        }),
    );
    let graph = render_text(&mut app, &mut terminal);
    assert!(graph.contains("Issue Graph: issue-1"), "rendered:\n{graph}");
    assert!(graph.contains("(issue-1)"), "rendered:\n{graph}");
    assert!(graph.contains("(issue-2)"), "rendered:\n{graph}");
    assert!(
        graph.contains("Legend: ○ open"),
        "tree renderer should display status legend:\n{graph}"
    );

    app.handle_crossterm_event_for_test(key('v'));
    let text = render_text(&mut app, &mut terminal);
    assert!(text.contains("Body for issue-1"), "rendered:\n{text}");
    assert!(text.contains("[Text] j/k: Nav"), "rendered:\n{text}");

    app.handle_crossterm_event_for_test(key('v'));
    assert!(
        rx.try_recv().is_err(),
        "cache hit must not send another GetIssueGraph"
    );
    let cached_graph = render_text(&mut app, &mut terminal);
    assert!(
        cached_graph.contains("Issue Graph: issue-1"),
        "rendered:\n{cached_graph}"
    );

    app.handle_crossterm_event_for_test(key('v'));
    let text = render_text(&mut app, &mut terminal);
    assert!(text.contains("Body for issue-1"), "rendered:\n{text}");

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(*app.current_view(), ViewId::IssueBrowser);
    let closed = render_text(&mut app, &mut terminal);
    assert!(
        closed.contains("Press Enter to view issue detail"),
        "rendered:\n{closed}"
    );
    assert!(!closed.contains("Body for issue-1"), "rendered:\n{closed}");
    assert!(
        !closed.contains("[Text]") && !closed.contains("[Graph]"),
        "list-only state must not show detail-mode status hint:\n{closed}"
    );
}

#[test]
fn stale_graph_response_for_non_current_request_is_ignored() {
    let (mut app, mut rx, mut terminal) = seeded_issue_browser_app();

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    expect_get_detail(&mut rx, "issue-1");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-1".into(),
            issue: sample_detail("issue-1", "First issue"),
        }),
    );
    app.handle_crossterm_event_for_test(key('v'));
    expect_get_graph(&mut rx, "issue-1");

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-2".into(),
            nodes: vec![graph_node("issue-2", "Stale issue")],
            edges: vec![],
        }),
    );
    let after_stale = render_text(&mut app, &mut terminal);
    assert!(
        after_stale.contains("Loading graph for issue-1"),
        "rendered:\n{after_stale}"
    );
    assert!(
        !after_stale.contains("Stale issue"),
        "rendered:\n{after_stale}"
    );

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-1".into(),
            nodes: vec![graph_node("issue-1", "Current issue")],
            edges: vec![],
        }),
    );
    let after_current = render_text(&mut app, &mut terminal);
    assert!(
        after_current.contains("Current issue"),
        "rendered:\n{after_current}"
    );
}

#[test]
fn esc_closes_graph_mode_in_one_press() {
    let (mut app, mut rx, mut terminal) = seeded_issue_browser_app();

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    expect_get_detail(&mut rx, "issue-1");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueDetailFetched {
            requested_id: "issue-1".into(),
            issue: sample_detail("issue-1", "First issue"),
        }),
    );
    app.handle_crossterm_event_for_test(key('v'));
    expect_get_graph(&mut rx, "issue-1");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-1".into(),
            nodes: vec![graph_node("issue-1", "Current issue")],
            edges: vec![],
        }),
    );
    let graph = render_text(&mut app, &mut terminal);
    assert!(graph.contains("Issue Graph: issue-1"), "rendered:\n{graph}");

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(*app.current_view(), ViewId::IssueBrowser);
    let closed = render_text(&mut app, &mut terminal);
    assert!(
        closed.contains("Press Enter to view issue detail"),
        "rendered:\n{closed}"
    );
    assert!(
        !closed.contains("Issue Graph: issue-1"),
        "rendered:\n{closed}"
    );
}
