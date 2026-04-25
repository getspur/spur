//! Render goldens for SessionPickerView. Inline `expected: &[&str]` per branch.
//! No external snapshot crate — diffs review as plain strings in PRs.

use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
use spur_acp::SessionInfo;
use spur_tui::session_metadata::SessionMetadata;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

const W: u16 = 80;
const H: u16 = 24;

fn buffer_to_lines(buf: &Buffer) -> Vec<String> {
    let mut out = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push(row.trim_end().to_string());
    }
    out
}

static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
    std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);

fn assert_render(picker: &mut SessionPickerView, expected: &[&str]) {
    let backend = TestBackend::new(W, H);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect::new(0, 0, W, H);
        let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
        picker.render(f, area, &ctx);
    })
    .unwrap();
    let lines = buffer_to_lines(term.backend().buffer());
    assert_eq!(
        lines.len(),
        expected.len(),
        "row count mismatch: actual {} vs expected {}",
        lines.len(),
        expected.len()
    );
    for (i, (got, want)) in lines.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "row {i} mismatch:\n  got:  {got:?}\n  want: {want:?}");
    }
}

fn session(id: &str, title: &str, cwd: &str) -> SessionInfo {
    SessionInfo::new(
        std::sync::Arc::<str>::from(id),
        std::path::PathBuf::from(cwd),
    )
    .title(title.to_string())
}

#[test]
fn populated_single_brain_no_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "claude".into(),
        vec![session("a1b2c3d4e5", "Refactor auth flow", "/work/spur")],
    );

    // Inline golden — capture current layout exactly. Any visual change to
    // session_picker.rs invalidates this and is reviewed as a plain string diff.
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search",
        "",
        "▸ + Start new session",
        "  ────",
        "  a1b2c3d4 · Refactor auth flow",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        " [↑↓]navigate [Enter]select [Esc]back                    ▶0 R0 $0.00 0m 00s spur",
        "j/k nav · Enter resume · / search · n new · R rename · d archive · a show-archiv",
    ];
    assert_render(&mut picker, expected);
}
