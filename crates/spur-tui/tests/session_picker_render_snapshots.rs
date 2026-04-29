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

fn synopsis() -> &'static spur_core::SessionSynopsisProjection {
    spur_tui::test_support::test_view_ctx(&LINEAGE).synopsis
}

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
    if lines.len() != expected.len()
        || lines
            .iter()
            .zip(expected.iter())
            .any(|(got, want)| got != want)
    {
        eprintln!("got:");
        for line in &lines {
            eprintln!("    {line:?},");
        }
    }
    assert_eq!(
        lines.len(),
        expected.len(),
        "row count mismatch: actual {} vs expected {}",
        lines.len(),
        expected.len()
    );
    for (i, (got, want)) in lines.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got, want,
            "row {i} mismatch:\n  got:  {got:?}\n  want: {want:?}"
        );
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
        synopsis(),
    );

    // Inline golden — capture current layout exactly. Any visual change to
    // session_picker.rs invalidates this and is reviewed as a plain string diff.
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "▸ Refactor auth flow    a1b2c3d4",
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
        "",
        "j/k nav · ↵ resume · / search · y yank · Esc             ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_empty_no_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions("claude".into(), vec![], synopsis());

    // Inline golden — captures the empty-list layout: only the [+ New]
    // virtual row + separator visible, status bar at line 22, footer at 23.
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search",
        "",
        "▸ + Start new session",
        "  ────",
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
        "",
        "",
        "j/k nav · ↵ new · / search · Esc                         ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn loading_state() {
    let mut picker = SessionPickerView::new();
    let expected: &[&str] = &[
        "",
        "",
        "",
        "",
        "",
        "",
        "Sessions",
        "",
        "  Connecting to agent ···",
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
        "Esc back                                                 ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn error_state() {
    let mut picker = SessionPickerView::new();
    picker.set_error("agent connection refused".into());
    let expected: &[&str] = &[
        "",
        "",
        "",
        "",
        "",
        "",
        "Sessions",
        "",
        "  agent connection refused",
        "  Use --resume <id> to load a session by ID.",
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
        "r retry · Esc back                                       ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_multi_brain_no_filter() {
    let mut picker = SessionPickerView::new();
    let mut meta = SessionMetadata::default();
    meta.sessions.entry("a1".into()).or_default().brain_name = Some("claude".into());
    meta.sessions.entry("a2".into()).or_default().brain_name = Some("gpt-5".into());
    meta.sessions
        .entry("a1xxxxxx".into())
        .or_default()
        .brain_name = Some("claude".into());
    meta.sessions
        .entry("a2xxxxxx".into())
        .or_default()
        .brain_name = Some("gpt-5".into());
    picker.set_metadata(meta);
    picker.set_sessions(
        "claude".into(),
        vec![
            session("a1xxxxxx", "Refactor auth", "/work/spur"),
            session("a2xxxxxx", "Tier 1 fixes", "/work/spur"),
        ],
        synopsis(),
    );
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "▸ Refactor auth  claude    a1xxxxxx",
        "  Tier 1 fixes  gpt-5    a2xxxxxx",
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
        "j/k nav · ↵ resume · / search · y yank · Esc             ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "claude".into(),
        vec![
            session("a1xxxxxx", "alpha", "/tmp"),
            session("a2xxxxxx", "beta", "/tmp"),
        ],
        synopsis(),
    );
    // Focus search and type 'b'.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE), &ctx);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE), &ctx);
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search  b_",
        "",
        "▸ + Start new session",
        "  ────",
        "  beta    a2xxxxxx",
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
        "",
        "type to filter · Enter commit · Esc exit search          ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_rename_active() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "t".into(),
        vec![session("a1xxxxxx", "alpha", "/tmp")],
        synopsis(),
    );
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    // Cursor on a1 by P1; press R to enter rename mode.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE), &ctx);
    assert!(picker.is_rename_active());
    let expected: &[&str] = &[
        "Sessions (t)",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "▸ alpha    a1xxxxxx",
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
        "Rename → alpha_",
        "type new title · Enter save · Esc cancel",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_confirm_switch() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "t".into(),
        vec![
            session("a1xxxxxx", "alpha", "/tmp"),
            session("a2xxxxxx", "beta", "/tmp"),
        ],
        synopsis(),
    );
    picker.set_current_session_has_draft(Some("a1".into()));
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    // Move cursor to a2 and press Enter — opens confirm-switch banner.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
    assert!(picker.is_confirm_switch_visible());
    let expected: &[&str] = &[
        "Sessions (t)",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  alpha    a1xxxxxx",
        "▸ beta    a2xxxxxx",
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
        "Session \"a1\" has an unsent draft — save and resume a2xxxxxx? [y/N]",
        "y/Enter confirm · n/Esc cancel",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_preview_visible() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "t".into(),
        vec![session("a1xxxxxx", "alpha", "/tmp")],
        synopsis(),
    );
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);
    assert!(picker.is_preview_visible());
    let expected: &[&str] = &[
        "Sessions (t)",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "▸ alpha    a1xxxxxx",
        "",
        "",
        "",
        "",
        "",
        " Preview ───────────────────────────────────────────────────────────────────────",
        "",
        "  (resume to load message history)",
        "",
        "  /tmp ·  · a1xxxxxx",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "j/k nav · ↵ resume · / search · y yank · Esc             ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_archived_shown() {
    let mut picker = SessionPickerView::new();
    let mut meta = SessionMetadata::default();
    meta.sessions.entry("a1".into()).or_default().archived = true;
    meta.sessions.entry("a1xxxxxx".into()).or_default().archived = true;
    picker.set_metadata(meta);
    picker.set_sessions(
        "t".into(),
        vec![session("a1xxxxxx", "alpha-archived", "/tmp")],
        synopsis(),
    );
    picker.toggle_show_archived(synopsis());
    let expected: &[&str] = &[
        "Sessions (t) [showing archived]",
        "  Search",
        "",
        "▸ + Start new session",
        "  ────",
        "  alpha-archived    a1xxxxxx [archived]",
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
        "",
        "j/k nav · ↵ new · / search · Esc                         ▶0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}
