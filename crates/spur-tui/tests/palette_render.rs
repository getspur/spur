use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};
use spur_tui::components::palette_overlay::PaletteOverlay;

fn render_to_string(state: &PaletteState, width: u16, height: u16) -> String {
    render_to_string_with_session_flag(state, width, height, true)
}

fn render_to_string_with_session_flag(
    state: &PaletteState,
    width: u16,
    height: u16,
    session_active: bool,
) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width, height };
        let overlay = PaletteOverlay::new(state).with_session_active(session_active);
        f.render_widget(overlay, area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn overlay_renders_title_query_and_rows() {
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "session · 2h ago".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "cmd · toggle plan".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    let rendered = render_to_string(&state, 60, 12);
    assert!(rendered.contains("Go to"), "title missing: {rendered}");
    assert!(rendered.contains("refactor-auth"), "session row missing");
    assert!(rendered.contains("/plan"), "command row missing");
    assert!(rendered.contains("$"), "session badge missing");
    assert!(rendered.contains(">"), "command badge missing");
}

#[test]
fn overlay_renders_empty_state_placeholder() {
    let state = PaletteState::new();
    let rendered = render_to_string(&state, 60, 12);
    assert!(rendered.contains("Go to"));
    assert!(rendered.contains("type to filter") || rendered.contains("no matches") || rendered.contains("No matches"));
}

#[test]
fn overlay_renders_no_match_hint_when_query_nonempty_and_ranked_empty() {
    // Non-empty query with no matches → "No matches. Try shorter or different keywords."
    let mut state = PaletteState::new();
    state.push_raw(vec![PaletteResult {
        kind: PaletteKind::Session,
        label: "zzz".into(),
        subtitle: "".into(),
        payload: PalettePayload::Session { session_id: "z".into() },
    }]);
    state.set_query("xyzzyfoobar");
    let rendered = render_to_string_with_session_flag(&state, 60, 12, true);
    assert!(
        rendered.contains("No matches"),
        "missing no-match hint; got:\n{rendered}"
    );
}

#[test]
fn overlay_renders_slash_hint_when_no_session_and_query_starts_with_slash() {
    // `/` prefix + no active session → hint about needing a session.
    let mut state = PaletteState::new();
    state.set_query("/something");
    let rendered = render_to_string_with_session_flag(&state, 60, 12, false);
    assert!(
        rendered.contains("Slash commands need an active session"),
        "missing slash-without-session hint; got:\n{rendered}"
    );
}
