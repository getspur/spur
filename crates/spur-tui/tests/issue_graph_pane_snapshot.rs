mod common;

use common::buffer_to_lines;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent};
use spur_tui::components::issue_graph_pane::IssueGraphPane;

const W: u16 = 72;
const H: u16 = 12;

fn node(id: &str, title: &str, status: &str) -> GraphNodeEvent {
    GraphNodeEvent {
        id: id.to_string(),
        title: Some(title.to_string()),
        status: Some(status.to_string()),
        priority: None,
        labels: Vec::new(),
        pagerank: None,
    }
}

fn edge(from: &str, to: &str, edge_type: &str) -> GraphEdgeEvent {
    GraphEdgeEvent {
        from: from.to_string(),
        to: to.to_string(),
        edge_type: Some(edge_type.to_string()),
    }
}

fn render_graph(
    pane: &mut IssueGraphPane,
    nodes: &[GraphNodeEvent],
    edges: &[GraphEdgeEvent],
    root_id: &str,
) -> Vec<String> {
    render_graph_at(pane, nodes, edges, root_id, W, H)
}

fn render_graph_at(
    pane: &mut IssueGraphPane,
    nodes: &[GraphNodeEvent],
    edges: &[GraphEdgeEvent],
    root_id: &str,
    width: u16,
    height: u16,
) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            pane.render(root_id, nodes, edges, frame, Rect::new(0, 0, width, height));
        })
        .unwrap();
    buffer_to_lines(terminal.backend().buffer())
}

fn render_loading(root_id: &str) -> Vec<String> {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            IssueGraphPane::render_loading(root_id, frame, Rect::new(0, 0, W, H));
        })
        .unwrap();
    buffer_to_lines(terminal.backend().buffer())
}

fn render_error(root_id: &str, message: &str) -> Vec<String> {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            IssueGraphPane::render_error(root_id, message, frame, Rect::new(0, 0, W, H));
        })
        .unwrap();
    buffer_to_lines(terminal.backend().buffer())
}

fn assert_snapshot(got: Vec<String>, expected: &[&str]) {
    if got.len() != expected.len()
        || got
            .iter()
            .zip(expected.iter())
            .any(|(got, want)| got != want)
    {
        eprintln!("got:");
        for line in &got {
            eprintln!("    {line:?},");
        }
    }

    assert_eq!(
        got.len(),
        expected.len(),
        "row count mismatch: actual {} vs expected {}",
        got.len(),
        expected.len()
    );
    for (i, (got, want)) in got.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got, want,
            "row {i} mismatch:\n  got:  {got:?}\n  want: {want:?}"
        );
    }
}

#[test]
fn renders_small_dependency_tree() {
    let mut pane = IssueGraphPane::new();
    let nodes = vec![
        node("bd-root", "Ship issue graph pane", "in_progress"),
        node("bd-api", "Define graph events", "closed"),
        node("bd-render", "Render adjacency tree", "open"),
        node("bd-cycle", "Detect cycles", "blocked"),
        node("bd-scroll", "Clamp viewport", "open"),
    ];
    let edges = vec![
        edge("bd-root", "bd-api", "blocks"),
        edge("bd-root", "bd-render", "blocks"),
        edge("bd-render", "bd-cycle", "blocks"),
        edge("bd-render", "bd-scroll", "blocks"),
        edge("bd-root", "bd-ignored", "relates"),
    ];

    assert_snapshot(
        render_graph(&mut pane, &nodes, &edges, "bd-root"),
        &[
            "┌ Issue Graph: bd-root  5 nodes ───────────────────────────────────────┐",
            "│● Ship issue graph pane (bd-root)                                     │",
            "│  ✅  Define graph events (bd-api)                                     │",
            "│  ○ Render adjacency tree (bd-render)                                 │",
            "│    ! Detect cycles (bd-cycle)                                        │",
            "│    ○ Clamp viewport (bd-scroll)                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│Legend: ○ open  ● in_progress  ! blocked  ✅  closed                   │",
            "└──────────────────────────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn renders_cycle_once_and_stops_expansion() {
    let mut pane = IssueGraphPane::new();
    let nodes = vec![
        node("bd-root", "Root", "open"),
        node("bd-a", "First dependency", "in_progress"),
        node("bd-b", "Back edge", "closed"),
    ];
    let edges = vec![
        edge("bd-root", "bd-a", "blocks"),
        edge("bd-a", "bd-b", "blocks"),
        edge("bd-b", "bd-root", "blocks"),
    ];

    assert_snapshot(
        render_graph(&mut pane, &nodes, &edges, "bd-root"),
        &[
            "┌ Issue Graph: bd-root  3 nodes ───────────────────────────────────────┐",
            "│○ Root (bd-root)                                                      │",
            "│  ● First dependency (bd-a)                                           │",
            "│    ✅  Back edge (bd-b)                                               │",
            "│      ○ Root (bd-root) ↻ cycle                                        │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│Legend: ○ open  ● in_progress  ! blocked  ✅  closed                   │",
            "└──────────────────────────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn renders_loading_state() {
    assert_snapshot(
        render_loading("bd-root"),
        &[
            "┌ Issue Graph: bd-root ────────────────────────────────────────────────┐",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                       Loading graph for bd-root                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "└──────────────────────────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn renders_error_state() {
    assert_snapshot(
        render_error("bd-root", "bv unavailable"),
        &[
            "┌ Issue Graph: bd-root ────────────────────────────────────────────────┐",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                      Graph error: bv unavailable                     │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "│                                                                      │",
            "└──────────────────────────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn scroll_down_past_end_clamps_to_content_height() {
    let mut pane = IssueGraphPane::new();
    let mut nodes = vec![node("bd-root", "Root", "open")];
    let mut edges = Vec::new();
    for i in 1..30 {
        let id = format!("bd-{i:02}");
        nodes.push(node(&id, &format!("Dependency {i:02}"), "open"));
        edges.push(edge("bd-root", &id, "blocks"));
    }

    let _ = render_graph_at(&mut pane, &nodes, &edges, "bd-root", 72, 10);
    pane.scroll_down_by(1000);
    pane.scroll_up_by(2);

    assert_snapshot(
        render_graph_at(&mut pane, &nodes, &edges, "bd-root", 72, 10),
        &[
            "┌ Issue Graph: bd-root  30 nodes ──────────────────────────────────────┐",
            "│  ○ Dependency 21 (bd-21)                                             │",
            "│  ○ Dependency 22 (bd-22)                                             │",
            "│  ○ Dependency 23 (bd-23)                                             │",
            "│  ○ Dependency 24 (bd-24)                                             │",
            "│  ○ Dependency 25 (bd-25)                                             │",
            "│  ○ Dependency 26 (bd-26)                                             │",
            "│↓ 3 more dependencies (PageDown)                                      │",
            "│Legend: ○ open  ● in_progress  ! blocked  ✅  closed                   │",
            "└──────────────────────────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn cjk_title_truncation_pads_to_right_border() {
    let mut pane = IssueGraphPane::new();
    let nodes = vec![node("bd-日本語", "Root", "open")];

    assert_snapshot(
        render_graph_at(&mut pane, &nodes, &[], "bd-日本語", 21, 2),
        &["┌ Issue Graph: bd-日 ┐", "└───────────────────┘"],
    );
}
