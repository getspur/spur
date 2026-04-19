//! Integration: rendered `PickerShell` popup rows apply atom styling
//! (LightBlue + UNDERLINED) to `ProtectedRange` byte spans, including
//! entries whose snapshot text contains newlines.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::components::picker_shell::PickerShell;
use spur_tui::components::query_source::HistoryQuerySource;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};

fn mk_entry(text: &str, ranges: Vec<ProtectedRange>) -> InputHistoryEntry {
    let mut snap = InputStateSnapshot::from_text(text);
    snap.protected_ranges = ranges;
    InputHistoryEntry::new(snap)
}

fn render_shell_and_extract_cells(
    shell: &mut PickerShell,
    width: u16,
    height: u16,
) -> Vec<(char, Color, Modifier)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let anchor = Rect::new(0, height - 1, width, 1);
            let container = Rect::new(0, 0, width, height);
            shell.render(f, anchor, container);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut cells = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let cell = buffer.cell((x, y)).expect("in-bounds buffer cell");
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            let fg = cell.style().fg.unwrap_or(Color::Reset);
            let modifier = cell.style().add_modifier;
            cells.push((ch, fg, modifier));
        }
    }
    cells
}

#[test]
fn atoms_render_with_light_blue_underlined_styling_no_newline() {
    let hist = vec![mk_entry(
        "hi @foo",
        vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }],
    )];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    let styled_count = cells
        .iter()
        .filter(|(_ch, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .count();
    assert!(
        styled_count >= 4,
        "expected >=4 LightBlue+UNDERLINED cells for @foo, got {styled_count}"
    );

    let styled_chars: String = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .map(|(c, _, _)| *c)
        .collect();
    assert!(
        styled_chars.contains("@foo"),
        "styled chars did not contain '@foo': {styled_chars:?}"
    );
}

#[test]
fn atoms_render_with_styling_across_newline_replacement() {
    let hist = vec![mk_entry(
        "hi\n@foo\nbye",
        vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }],
    )];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    let styled_chars: String = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .map(|(c, _, _)| *c)
        .collect();
    assert!(
        styled_chars.contains("@foo"),
        "styled chars did not contain '@foo' on multi-line entry: {styled_chars:?}"
    );
    assert!(
        !styled_chars.contains('↵'),
        "↵ newline glyph should not be in atom styling: {styled_chars:?}"
    );
}

#[test]
fn entry_without_atoms_has_no_light_blue_cells() {
    let hist = vec![mk_entry("no mentions here", vec![])];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    let styled_count = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .count();
    assert_eq!(styled_count, 0);
}
