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

    // Two-line rows: line 1 = title (padded to label_budget) + time column;
    // line 2 = activity placeholder. Short id removed from list row.
    // Header counter appears after (agent). Group header EARLIER shown.
    // Preview is on by default; no synopsis → placeholder row shown.
    let expected: &[&str] = &[
        "Sessions (claude) \u{b7} 1 session",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  EARLIER",
        "\u{25b8} Refactor auth flow",
        "      \u{2514} resume to load message history",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "  (resume to load message history)",
        "",
        "  /work/spur \u{b7}  \u{b7} a1b2c3d4",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "j/k nav \u{b7} \u{21b5} resume \u{b7} / search \u{b7} y yank \u{b7} Esc             \u{25b6}0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_empty_no_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions("claude".into(), vec![], synopsis());

    // Empty session list: no group headers, count shows 0.
    // Preview is on by default; cursor=0 shows the "new session" placeholder.
    let expected: &[&str] = &[
        "Sessions (claude) \u{b7} 0 sessions",
        "  Search",
        "",
        "\u{25b8} + Start new session",
        "  ────",
        "",
        "",
        "",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "Press Enter to start a new session \u{b7} any unsent draft will be saved",
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
        "j/k nav \u{b7} \u{21b5} new \u{b7} / search \u{b7} Esc                         \u{25b6}0 R0 $0.00 0m 00s spur",
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
        "  Connecting to agent \u{b7}\u{b7}\u{b7}",
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
        "Esc back                                                 \u{25b6}0 R0 $0.00 0m 00s spur",
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
        "r retry \u{b7} Esc back                                       \u{25b6}0 R0 $0.00 0m 00s spur",
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
    // Two-line rows with brain column. Short id removed. Group header EARLIER shown.
    // With H=24 + preview(12) + status(1) the list viewport holds ~1 session;
    // cursor on a1xxxxxx (first) → only that row shown.
    // Preview is on by default; cursor on a1xxxxxx (no synopsis) → placeholder.
    let expected: &[&str] = &[
        "Sessions (claude) \u{b7} 2 sessions",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  EARLIER",
        "\u{25b8} Refactor auth                                                claude",
        "      \u{2514} resume to load message history",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "  (resume to load message history)",
        "",
        "  /work/spur \u{b7} claude \u{b7} a1xxxxxx",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "j/k nav \u{b7} \u{21b5} resume \u{b7} / search \u{b7} y yank \u{b7} Esc             \u{25b6}0 R0 $0.00 0m 00s spur",
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
    // Filter active: no group headers. Two-line rows shown. Short id removed.
    // Count shows ALL unfiltered sessions even when filter is active.
    // Preview is on by default; cursor=0 (new session) with filter active.
    let expected: &[&str] = &[
        "Sessions (claude) \u{b7} 2 sessions",
        "  Search  b_",
        "",
        "\u{25b8} + Start new session",
        "  ────",
        "  beta",
        "      \u{2514} resume to load message history",
        "",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "Press Enter to start a new session \u{b7} any unsent draft will be saved",
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
        "type to filter \u{b7} Enter commit \u{b7} Esc exit search          \u{25b6}0 R0 $0.00 0m 00s spur",
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
    // Two-line rows. Group header EARLIER. Preview is on by default; rename prompt displaces status bar.
    let expected: &[&str] = &[
        "Sessions (t) \u{b7} 1 session",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  EARLIER",
        "\u{25b8} alpha",
        "      \u{2514} resume to load message history",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "  (resume to load message history)",
        "",
        "  /tmp \u{b7}  \u{b7} a1xxxxxx",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Rename \u{2192} alpha_",
        "type new title \u{b7} Enter save \u{b7} Esc cancel",
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
    // Two-line rows. Group header EARLIER. Preview on; confirm-switch prompt displaces status bar.
    // With H=24 + preview(12) + status(1) + footer(1) the viewport holds ~1 session;
    // cursor on beta → scroll clamps so only beta is visible.
    let expected: &[&str] = &[
        "Sessions (t) \u{b7} 2 sessions",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  EARLIER",
        "\u{25b8} beta",
        "      \u{2514} resume to load message history",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "  (resume to load message history)",
        "",
        "  /tmp \u{b7}  \u{b7} a2xxxxxx",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Session \"a1\" has an unsent draft \u{2014} save and resume a2xxxxxx? [y/N]",
        "y/Enter confirm \u{b7} n/Esc cancel",
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
    let _ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    // Preview is on by default — no key press needed.
    assert!(picker.is_preview_visible());
    // Same layout as rename_active but without rename prompt.
    let expected: &[&str] = &[
        "Sessions (t) \u{b7} 1 session",
        "  Search",
        "",
        "  + Start new session",
        "  ────",
        "  EARLIER",
        "\u{25b8} alpha",
        "      \u{2514} resume to load message history",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "  (resume to load message history)",
        "",
        "  /tmp \u{b7}  \u{b7} a1xxxxxx",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "j/k nav \u{b7} \u{21b5} resume \u{b7} / search \u{b7} y yank \u{b7} Esc             \u{25b6}0 R0 $0.00 0m 00s spur",
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
    // Two-line rows. Archived count + [showing archived] in header.
    // [archived] badge on title line. Short id removed.
    // Preview is on by default; cursor=0 (new session) shows new-session placeholder.
    let expected: &[&str] = &[
        "Sessions (t) \u{b7} 1 session \u{b7} 2 archived [showing archived]",
        "  Search",
        "",
        "\u{25b8} + Start new session",
        "  ────",
        "  EARLIER",
        "  alpha-archived                                               [archived]",
        "      \u{2514} resume to load message history",
        "",
        "",
        "",
        " Preview \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "Press Enter to start a new session \u{b7} any unsent draft will be saved",
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
        "j/k nav \u{b7} \u{21b5} new \u{b7} / search \u{b7} Esc                         \u{25b6}0 R0 $0.00 0m 00s spur",
    ];
    assert_render(&mut picker, expected);
}

/// Regression test for scroll clamp correctness with preview pane open.
///
/// With H=24 and preview_height=12 the list pane has only ~11 rows; after
/// subtracting chrome and header-reserve that fits ~1 session (2 lines each).
/// Moving the cursor to the last of 8 sessions must scroll so BOTH of its
/// lines appear in the rendered buffer.  The [+ Start new session] row must
/// also remain permanently visible (it is always the first rendered row after
/// the chrome header).
///
/// This test was written to FAIL under the pre-fix clamp (which used
/// area.height - 4 and ignored preview/status rows, giving visible_sessions~=8
/// so the clamp never fired) and PASS after the fix.
#[test]
fn selected_session_fully_visible_at_small_viewport() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());

    // 8 sessions -- more than can fit with preview open on a 24-row terminal.
    let sessions: Vec<SessionInfo> = (1u8..=8)
        .map(|i| {
            session(
                &format!("session{:02}xxxxxxxx", i),
                &format!("Session {i}"),
                "/tmp",
            )
        })
        .collect();
    picker.set_sessions("t".into(), sessions, synopsis());
    assert!(picker.is_preview_visible());

    // Move cursor to the last session (cursor = 8).
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    for _ in 0..8 {
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
    }
    assert_eq!(
        picker.cursor(),
        8,
        "cursor should be on last session (index 8)"
    );

    // Render into a 80x24 terminal.
    let backend = TestBackend::new(W, H);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect::new(0, 0, W, H);
        picker.render(f, area, &ctx);
    })
    .unwrap();

    let lines = buffer_to_lines(term.backend().buffer());

    // The [+ Start new session] row must always be visible (fixed chrome).
    assert!(
        lines.iter().any(|l| l.contains("+ Start new session")),
        "[+ Start new session] must be visible; lines:\n{lines:#?}"
    );

    // Both lines of the selected session ("Session 8") must appear.
    let title_visible = lines
        .iter()
        .any(|l| l.contains('\u{25b8}') && l.contains("Session 8"));
    assert!(
        title_visible,
        "selected session title line must be visible; lines:\n{lines:#?}"
    );

    let activity_visible = lines.iter().any(|l| l.contains('\u{2514}'));
    assert!(
        activity_visible,
        "selected session activity line must be visible; lines:\n{lines:#?}"
    );
}
