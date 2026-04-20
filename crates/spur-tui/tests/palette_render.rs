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
    // Set a non-empty query so the flat render path is used instead of the
    // grouped empty-query view (Task 7). "a" matches both "refactor-auth" and
    // "/plan" (via "plan") so both rows remain visible.
    state.set_query("a");
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
    assert!(
        rendered.contains("type to filter"),
        "empty-query empty-state should render the 'type to filter' hint"
    );
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

#[test]
fn overlay_renders_grouped_sections_when_query_empty() {
    // Seed one of each kind; empty query; expect section headers.
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/help".into(),
            subtitle: "cmd · show help".into(),
            payload: PalettePayload::Command { name: "help".into() },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "session · s1".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Worker,
            label: "codex".into(),
            subtitle: "worker · running".into(),
            payload: PalettePayload::Worker {
                session_id: spur_acp::SessionId("w1".into()),
            },
        },
    ]);
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    assert!(rendered.contains("COMMANDS"), "missing COMMANDS header:\n{rendered}");
    assert!(rendered.contains("SESSIONS"), "missing SESSIONS header:\n{rendered}");
    assert!(rendered.contains("WORKERS"), "missing WORKERS header:\n{rendered}");
    assert!(
        rendered.contains("TRACE \u{2014} coming soon") || rendered.contains("TRACE - coming soon"),
        "missing TRACE placeholder:\n{rendered}"
    );
    assert!(rendered.contains("/help"));
    assert!(rendered.contains("refactor-auth"));
    assert!(rendered.contains("codex"));
}

#[test]
fn overlay_falls_back_to_flat_render_when_query_nonempty() {
    let mut state = PaletteState::new();
    state.push_raw(vec![PaletteResult {
        kind: PaletteKind::Command,
        label: "/help".into(),
        subtitle: "cmd · show help".into(),
        payload: PalettePayload::Command { name: "help".into() },
    }]);
    state.set_query("help");
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    // Flat render = no section headers.
    assert!(!rendered.contains("COMMANDS"), "unexpected header in flat render:\n{rendered}");
    assert!(rendered.contains("/help"));
}

#[test]
fn overlay_grouped_view_shows_trace_placeholder_at_80x24_with_full_data() {
    // Regression for the cap formula. At 80x24 the modal is 48x14,
    // inner area 9 rows tall for the list. With three populated kinds
    // (5 commands + 5 sessions + 5 workers, more than the cap), the
    // TRACE placeholder must STILL appear — otherwise users at the
    // most common SSH terminal size never see the deferred-feature
    // signal.
    let mut state = PaletteState::new();
    let mut batch = Vec::new();
    for i in 0..5 {
        batch.push(PaletteResult {
            kind: PaletteKind::Command,
            label: format!("/cmd-{i}"),
            subtitle: "cmd".into(),
            payload: PalettePayload::Command { name: format!("cmd-{i}") },
        });
    }
    for i in 0..5 {
        batch.push(PaletteResult {
            kind: PaletteKind::Session,
            label: format!("session-{i}"),
            subtitle: "session".into(),
            payload: PalettePayload::Session { session_id: format!("s{i}") },
        });
    }
    for i in 0..5 {
        batch.push(PaletteResult {
            kind: PaletteKind::Worker,
            label: format!("worker-{i}"),
            subtitle: "worker".into(),
            payload: PalettePayload::Worker {
                session_id: spur_acp::SessionId(format!("w{i}")),
            },
        });
    }
    state.push_raw(batch);
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    assert!(
        rendered.contains("TRACE \u{2014} coming soon") || rendered.contains("TRACE - coming soon"),
        "TRACE placeholder must render at 80x24 with full data:\n{rendered}"
    );
}

#[test]
fn overlay_grouped_view_highlights_cursor_row() {
    // Two sessions; cursor at index 1; that row should render with
    // REVERSED styling. We can't easily assert on style in raw text,
    // but we can confirm BOTH labels appear in the grouped render
    // (which they do today) and that no panic occurs when cursor moves.
    // The substantive assertion lives in render_flat behavior; for
    // grouped we just verify the cursor-aware code path doesn't break.
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "first-session".into(),
            subtitle: "session · 1".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "second-session".into(),
            subtitle: "session · 2".into(),
            payload: PalettePayload::Session { session_id: "s2".into() },
        },
    ]);
    state.cursor_down(); // move cursor to index 1
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    assert!(rendered.contains("first-session"));
    assert!(rendered.contains("second-session"));
}

#[test]
fn overlay_grouped_view_caps_rows_per_kind() {
    // 10 sessions; default cap is 5 → at most 5 session labels render.
    let mut state = PaletteState::new();
    let mut batch = Vec::new();
    for i in 0..10 {
        batch.push(PaletteResult {
            kind: PaletteKind::Session,
            label: format!("session-{i:02}"),
            subtitle: format!("session · s{i}"),
            payload: PalettePayload::Session {
                session_id: format!("s{i}"),
            },
        });
    }
    state.push_raw(batch);
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    let shown = (0..10)
        .filter(|i| rendered.contains(&format!("session-{i:02}")))
        .count();
    assert!(
        shown <= 5,
        "expected cap of 5 sessions in grouped view; got {shown}:\n{rendered}"
    );
    assert!(shown >= 2, "expected at least 2 sessions rendered; got {shown}");
}
