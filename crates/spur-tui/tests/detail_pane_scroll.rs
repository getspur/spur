//! Integration tests for `DetailPane` scroll + badge correctness.
//!
//! These pin the behavior that the footer "▼ following" indicator
//! reflects `ReactTrace.anchor` (not a phantom pane-local flag) when the
//! Stream tab is active, and that `cycle_tab` opens non-Stream tabs at
//! the top of their content (not at the bottom, as it does today because
//! `is_following = true` is reconciled against `scroll_offset = 0` at
//! render time in favor of following).

use std::collections::VecDeque;
use std::time::SystemTime;

use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
use spur_acp::domain::events::{LifecycleState, Role};
use spur_acp::{AgentKind, SessionId};
use spur_core::lineage::{Attempt, AttemptStatus, ExecutorId, ExecutorNode};
use spur_tui::components::detail_pane::{DetailPane, DetailTab};
use spur_tui::components::react_trace::ReactTrace;

/// Build a minimal `ExecutorNode` suitable for exercising `DetailPane::render`.
fn node(task_spec: &str) -> ExecutorNode {
    ExecutorNode {
        id: ExecutorId::new("exec-test".to_string()),
        parent_id: None,
        child_ids: Vec::new(),
        agent: "claude".to_string(),
        role: Role::Brain,
        task_spec: task_spec.to_string(),
        phase: LifecycleState::Running,
        attempts: vec![Attempt {
            session_id: SessionId("sess-0".to_string()),
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

/// Return true iff the given `Buffer` contains the substring `needle`
/// anywhere in its cell content. Used to inspect border rows for the
/// "▼ following" footer indicator.
fn buffer_contains(buf: &Buffer, needle: &str) -> bool {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            s.push_str(cell.symbol());
        }
        s.push('\n');
    }
    s.contains(needle)
}

/// Seed a compact `ReactTrace` with enough entries to overflow the viewport,
/// render once so `compact_cache` + `entry_row_starts` are populated, and
/// return it parked at `Following`.
fn seeded_compact_trace() -> ReactTrace {
    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..20 {
        trace.append_think(&format!("th-{}", i), "12:00".into());
        trace.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }
    let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 80, 10)))
        .unwrap();
    trace
}

/// T-D3a: On the Stream tab, the footer "▼ following" indicator must
/// reflect the pane's `is_following` flag, which is authoritative for
/// scroll state now that DetailPane manages its own viewport.
#[test]
fn stream_footer_badge_reflects_pane_follow_state() {
    let mut pane = DetailPane::new();
    assert_eq!(pane.current_tab(), DetailTab::Stream);
    let mut trace = seeded_compact_trace();

    // Render once while following: badge should appear.
    let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
    term.draw(|f| {
        pane.render(
            f,
            Rect::new(0, 0, 80, 12),
            &node(""),
            None,
            Some(&mut trace),
        );
    })
    .unwrap();
    let frame_following = term.backend().buffer().clone();
    assert!(
        buffer_contains(&frame_following, "▼ following"),
        "while Following, badge must appear in the rendered buffer:\n{:#?}",
        frame_following
    );

    // Scroll up — pane leaves Following.
    pane.scroll_up();
    assert!(
        !pane.is_following(),
        "scroll_up should take pane off Following"
    );

    // Re-render: badge must NOT appear.
    term.draw(|f| {
        pane.render(
            f,
            Rect::new(0, 0, 80, 12),
            &node(""),
            None,
            Some(&mut trace),
        );
    })
    .unwrap();
    let frame_scrolled = term.backend().buffer().clone();
    assert!(
        !buffer_contains(&frame_scrolled, "▼ following"),
        "after scroll_up, badge must not appear (pane footer would lie):\n{:#?}",
        frame_scrolled
    );
}

/// T-D3b: On the Stream tab with `stream_trace = None` (placeholder state
/// before the first trace materializes), the footer shows the same
/// content-fits indicator as any other tab with short content.
#[test]
fn stream_footer_badge_placeholder_shows_content_fits() {
    let mut pane = DetailPane::new();
    let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
    term.draw(|f| {
        pane.render(f, Rect::new(0, 0, 80, 12), &node(""), None, None);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    // Placeholder is one line "(no stream yet)"; viewport is 10 rows,
    // so max_offset == 0 and the label is the generic "▼".
    assert!(
        buffer_contains(&buf, "▼"),
        "Stream placeholder path must show '▼' footer:\n{:#?}",
        buf
    );
}

/// T-D4a: Cycling from Stream into a non-Stream tab (e.g. Task) must open
/// that tab at the TOP of its content, not at the bottom. Today the pane
/// sets `scroll_offset = 0` and `is_following = true`, and the render
/// re-clamps to `max_offset` (bottom) because following wins.
#[test]
fn cycle_tab_opens_task_at_top() {
    let mut pane = DetailPane::new();
    // Task spec with enough lines to overflow the viewport.
    let task_spec: String = (0..40)
        .map(|i| format!("line-{}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let n = node(&task_spec);

    // Cycle Stream → Artifacts → Attempts → Task.
    for _ in 0..3 {
        pane.cycle_tab(true);
    }
    assert_eq!(pane.current_tab(), DetailTab::Task);

    // Render at height 8; visible content should include the FIRST task line.
    let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
    term.draw(|f| {
        pane.render(f, Rect::new(0, 0, 40, 8), &n, None, None);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    assert!(
        buffer_contains(&buf, "line-0"),
        "freshly-entered non-Stream tab must render from the top; `line-0` missing:\n{:#?}",
        buf
    );
    assert!(
        !buffer_contains(&buf, "line-39"),
        "freshly-entered non-Stream tab must NOT already be at the bottom; `line-39` present:\n{:#?}",
        buf
    );
}

/// T-D2a: A non-Stream tab whose content contains a line longer than
/// the pane width must let the user scroll down to the wrapped tail of
/// that line. Today `max_offset` is computed from unwrapped
/// `body_lines.len()`, so very long single-line content is unscrollable
/// past its first visible wrap.
#[test]
fn non_stream_scroll_reaches_wrapped_bottom() {
    let mut pane = DetailPane::new();
    // Task spec: a very long single line, so `lines().count() == 1` but
    // `wrap_line_to_width(_, ~38) ` produces many rows. The "ZZZZEND"
    // marker lives at the tail of the wrapped content.
    let task_spec = format!("{}ZZZZEND", "x".repeat(500));
    let n = node(&task_spec);

    // Cycle Stream → Artifacts → Attempts → Task.
    for _ in 0..3 {
        pane.cycle_tab(true);
    }
    assert_eq!(pane.current_tab(), DetailTab::Task);

    // Terminal 40 × 10: body is ~38 × 7 after borders/tab bar.
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| pane.render(f, Rect::new(0, 0, 40, 10), &n, None, None))
        .unwrap();

    // Scroll down repeatedly — must reach the wrapped tail. Render between
    // each scroll so the pane's max_offset is re-clamped using whatever
    // total-row count the render path sees.
    for _ in 0..50 {
        pane.scroll_down();
        term.draw(|f| pane.render(f, Rect::new(0, 0, 40, 10), &n, None, None))
            .unwrap();
    }

    let buf = term.backend().buffer().clone();
    assert!(
        buffer_contains(&buf, "ZZZZEND"),
        "after 50× scroll_down on a Task tab with a long wrapped line, the \
         tail marker `ZZZZEND` must be visible; buffer:\n{:#?}",
        buf
    );
}

/// T-D4b: Cycling back into the Stream tab from elsewhere must snap the
/// pane back to Following so the user sees the latest output — otherwise
/// returning to Stream may leave the viewport pinned mid-history.
#[test]
fn cycle_tab_into_stream_snaps_pane_to_following() {
    let mut pane = DetailPane::new();
    let _trace = seeded_compact_trace();

    // Walk away from Stream (Artifacts).
    pane.cycle_tab(true);
    assert_ne!(pane.current_tab(), DetailTab::Stream);

    // Return to Stream, scroll up, then leave again so the "snap back"
    // is observable on re-entry.
    while pane.current_tab() != DetailTab::Stream {
        pane.cycle_tab(true);
    }
    pane.scroll_up();
    assert!(!pane.is_following());

    // Walk away and cycle back around to Stream — must re-engage Following.
    pane.cycle_tab(true);
    while pane.current_tab() != DetailTab::Stream {
        pane.cycle_tab(true);
    }
    assert!(
        pane.is_following(),
        "entering the Stream tab must re-engage Following"
    );
}
