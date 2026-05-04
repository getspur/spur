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
    IssueSummaryEvent {
        id: id.into(),
        source: "beads".into(),
        title: title.into(),
        status: "open".into(),
        labels: Vec::new(),
        priority: Some(1),
        issue_type: Some("task".into()),
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
    GraphNodeEvent {
        id: id.into(),
        title: Some(title.into()),
        status: Some("open".into()),
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

    expect_get_graph(&mut rx, "issue-1");

    app.handle_crossterm_event_for_test(key('j'));

    expect_get_graph(&mut rx, "issue-2");
}

#[test]
fn issue_list_renders_lineage_context_from_selected_graph_cache() {
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
                sample_summary("issue-A", "Selected issue"),
                sample_summary("issue-B", "Upstream blocker"),
                sample_summary("issue-C", "Downstream work"),
            ],
        }),
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-A".into(),
            nodes: vec![
                graph_node("issue-A", "Selected issue"),
                graph_node("issue-B", "Upstream blocker"),
                graph_node("issue-C", "Downstream work"),
            ],
            edges: vec![
                graph_edge("issue-B", "issue-A"),
                graph_edge("issue-A", "issue-C"),
            ],
        }),
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    let rendered = render_text(&mut app, &mut terminal);

    assert!(
        rendered.contains("Work Item Lineage"),
        "rendered:\n{rendered}"
    );
    assert!(rendered.contains("issue-B"), "rendered:\n{rendered}");
    assert!(rendered.contains("issue-A"), "rendered:\n{rendered}");
    assert!(rendered.contains("issue-C"), "rendered:\n{rendered}");
    assert!(
        rendered.contains("blocked by open upstream"),
        "rendered:\n{rendered}"
    );
}

#[test]
fn issue_list_keeps_lineage_panel_while_selected_graph_is_loading() {
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
                sample_summary("issue-A", "Selected issue"),
                sample_summary("issue-B", "Next issue"),
                sample_summary("issue-C", "Downstream work"),
            ],
        }),
    );
    expect_get_graph(&mut rx, "issue-A");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "issue-A".into(),
            nodes: vec![
                graph_node("issue-A", "Selected issue"),
                graph_node("issue-C", "Downstream work"),
            ],
            edges: vec![graph_edge("issue-A", "issue-C")],
        }),
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    let before_nav = render_text(&mut app, &mut terminal);
    assert!(
        before_nav.contains("Work Item Lineage"),
        "rendered:\n{before_nav}"
    );

    app.handle_crossterm_event_for_test(key('j'));

    expect_get_graph(&mut rx, "issue-B");
    let after_nav = render_text(&mut app, &mut terminal);
    assert!(
        after_nav.contains("Work Item Lineage"),
        "lineage mode should persist while the newly-selected graph loads:\n{after_nav}"
    );
    assert!(
        after_nav.contains("loading work tree"),
        "loading state should be explicit instead of falling back to the flat list:\n{after_nav}"
    );
}

#[test]
fn issue_list_keeps_plan_epic_as_root_when_child_is_selected() {
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
                sample_summary("bd-2pb", "Epic root"),
                sample_summary("bd-2pb.1", "P2 child"),
                sample_summary("bd-2pb.2", "P3 child"),
            ],
        }),
    );
    expect_get_graph(&mut rx, "bd-2pb");
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "bd-2pb".into(),
            nodes: vec![
                graph_node("bd-2pb", "Epic root"),
                graph_node("bd-2pb.1", "P2 child"),
                graph_node("bd-2pb.2", "P3 child"),
            ],
            edges: vec![
                graph_edge_with_type("bd-2pb.1", "bd-2pb", "related"),
                graph_edge_with_type("bd-2pb.2", "bd-2pb", "related"),
                graph_edge("bd-2pb.2", "bd-2pb.1"),
            ],
        }),
    );

    app.handle_crossterm_event_for_test(key('j'));

    expect_get_graph(&mut rx, "bd-2pb.1");
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    let loading = render_text(&mut app, &mut terminal);
    assert!(
        loading.contains("loading work tree"),
        "selected child should keep the lineage pane during graph loading:\n{loading}"
    );
    assert!(
        loading.contains("> ○ bd-2pb"),
        "epic must remain the lineage root during child graph loading:\n{loading}"
    );
    assert!(
        loading.contains("├─ ○ bd-2pb.1") || loading.contains("└─ ○ bd-2pb.1"),
        "selected child must remain under the epic root during graph loading:\n{loading}"
    );
    assert!(
        !loading.contains("> ○ bd-2pb.1"),
        "selected child must not become the loading lineage root:\n{loading}"
    );

    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
            requested_id: "bd-2pb.1".into(),
            nodes: vec![
                graph_node("bd-2pb", "Epic root"),
                graph_node("bd-2pb.1", "P2 child"),
            ],
            edges: vec![graph_edge_with_type("bd-2pb.1", "bd-2pb", "related")],
        }),
    );

    let rendered = render_text(&mut app, &mut terminal);

    assert!(
        rendered.contains("Work Item Lineage"),
        "rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("> ○ bd-2pb"),
        "epic must remain the lineage root when a child row is selected:\n{rendered}"
    );
    assert!(
        rendered.contains("├─ ○ bd-2pb.1") || rendered.contains("└─ ○ bd-2pb.1"),
        "selected child must render under the epic root:\n{rendered}"
    );
    assert!(
        !rendered.contains("> ○ bd-2pb.1"),
        "selected child must not become the lineage root:\n{rendered}"
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
