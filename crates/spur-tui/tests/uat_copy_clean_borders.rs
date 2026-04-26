mod common;

use crossterm::event::KeyCode;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::input_bar::{EditMode, VimMode};
use spur_tui::components::react_trace::ReactTrace;

use common::{
    assert_no_glyphs, assert_no_vertical_border_glyphs, buffer_text, row_text, TestHarness,
};

fn trace_with_lines(count: usize) -> ReactTrace {
    let mut trace = ReactTrace::new();
    for i in 1..=count {
        if i % 2 == 0 {
            trace.append_message(&format!("trace line {i}"), "codex", "12:00".into());
        } else {
            trace.append_think(&format!("trace line {i}"), "12:00".into());
        }
    }
    trace
}

fn render_trace(trace: &mut ReactTrace, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| trace.render(f, Rect::new(0, 0, width, height), None))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn f1_u1_no_vertical_glyphs_in_dashboard_render() {
    let mut trace = ReactTrace::new();
    trace.append_message(
        "copy this line\nand this line\nwithout side rails",
        "codex",
        "12:00".into(),
    );

    let width = 80;
    let height = 8;
    let buf = render_trace(&mut trace, width, height);

    assert_no_vertical_border_glyphs(&buf, width, height);
    assert_no_glyphs(&buf, width, height, &["║", "█"]);
}

#[test]
fn f1_u2_overflow_shows_position_indicator_no_thumb() {
    let width = 80;
    let height = 8;
    let mut trace = trace_with_lines(40);

    let buf = render_trace(&mut trace, width, height);
    let text = buffer_text(&buf);

    assert!(
        row_text(&buf, height - 1).contains('%'),
        "bottom border should show position indicator, got:\n{text}"
    );
    assert!(
        !text.contains('█'),
        "copy-friendly overflow indicator should not render a scrollbar thumb:\n{text}"
    );
}

#[test]
fn f1_u3_no_overflow_shows_no_indicator() {
    let width = 80;
    let height = 12;
    let mut trace = trace_with_lines(2);

    let buf = render_trace(&mut trace, width, height);
    let bottom = row_text(&buf, height - 1);

    assert!(
        !bottom.contains('%'),
        "short content should not show position indicator on bottom row: {bottom:?}"
    );
}

#[test]
fn f1_u4_vim_visual_mode_renders_glyph_prefix_in_badge() {
    let mut h = TestHarness::new(80, 24);

    h.app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test()
        .set_mode_for_test(EditMode::Vim(VimMode::Visual));
    h.send_key(KeyCode::Char('x'));
    h.render();

    let text = h.buffer_text();
    assert!(
        text.contains('▦') || text.contains("[VISUAL]"),
        "Visual mode badge should include its glyph prefix or label, got:\n{text}"
    );
}
