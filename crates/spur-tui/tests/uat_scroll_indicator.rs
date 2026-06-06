mod common;

use std::collections::VecDeque;
use std::time::SystemTime;

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::{LifecycleState, Role, SessionId};
use spur_core::{Attempt, AttemptStatus, ExecutorId, ExecutorNode};
use spur_tui::components::detail_pane::{DetailPane, DetailTab};

use common::{buffer_text, row_text};

fn node_with_task_lines(count: usize) -> ExecutorNode {
    ExecutorNode {
        id: ExecutorId::new("exec-scroll".to_string()),
        parent_id: None,
        child_ids: Vec::new(),
        agent: "codex".to_string(),
        role: Role::Executor,
        task_spec: (1..=count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
        phase: LifecycleState::Running,
        attempts: vec![Attempt {
            session_id: SessionId("sess-scroll".to_string()),
            started_at: SystemTime::UNIX_EPOCH,
            ended_at: None,
            status: AttemptStatus::Running,
            cost_usd: 0.0,
            artifacts: Vec::new(),
            error: None,
        }],
        pending_review: None,
        last_event_at: None,
        tool_call_count: 0,
        latest_tool_call: None,
        latest_progress_message: None,
        latest_progress_percent: None,
        files_touched_count: 0,
        latest_diff_summary: None,
        latest_diff_text: None,
        last_error: None,
        stream_buffer: VecDeque::new(),
        issue_id: None,
        delegation_id: None,
        peer_edges: Vec::new(),
    }
}

fn render_pane(pane: &mut DetailPane, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let node = node_with_task_lines(27);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| pane.render(f, Rect::new(0, 0, width, height), &node, None, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn task_pane() -> DetailPane {
    let mut pane = DetailPane::new();
    pane.jump_to_tab(DetailTab::Task);
    pane
}

#[test]
fn f2_u1_top_of_viewport_indicator() {
    let mut pane = task_pane();

    let buf = render_pane(&mut pane, 70, 8);

    assert!(
        row_text(&buf, 7).contains(" · 5/27 · 18% "),
        "top viewport indicator missing, got:\n{}",
        buffer_text(&buf)
    );
}

#[test]
fn f2_u2_mid_viewport_indicator() {
    let mut pane = task_pane();
    pane.scroll_down_by(7);

    let buf = render_pane(&mut pane, 70, 8);

    assert!(
        row_text(&buf, 7).contains(" · 12/27 · 44% "),
        "middle viewport indicator missing, got:\n{}",
        buffer_text(&buf)
    );
}

#[test]
fn f2_u3_bottom_viewport_100_percent() {
    let mut pane = task_pane();
    pane.scroll_to_bottom();

    let buf = render_pane(&mut pane, 70, 8);

    assert!(
        row_text(&buf, 7).contains(" · 27/27 · 100% "),
        "bottom viewport indicator missing, got:\n{}",
        buffer_text(&buf)
    );
}

#[test]
fn f2_u4_narrow_pane_compact_then_hidden() {
    let mut compact = task_pane();
    compact.scroll_down_by(10);
    let compact_buf = render_pane(&mut compact, 25, 8);
    assert!(
        row_text(&compact_buf, 7).contains(" · 55% "),
        "compact percentage indicator missing, got:\n{}",
        buffer_text(&compact_buf)
    );

    let mut hidden = task_pane();
    hidden.scroll_down_by(10);
    let hidden_buf = render_pane(&mut hidden, 19, 8);
    assert!(
        !row_text(&hidden_buf, 7).contains('%'),
        "indicator should be hidden below 20 columns, got:\n{}",
        buffer_text(&hidden_buf)
    );
}
