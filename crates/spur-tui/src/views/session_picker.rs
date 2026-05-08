use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{SessionInfo, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;
use crate::session_metadata::SessionMetadata;
use crate::theme::{resolve_token, ColorDepth, Theme};

use super::View;

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

const PREVIEW_MAX_LINES: u16 = 12;

fn footer_hint(state: &PickerState, rename_active: bool, confirm_active: bool) -> &'static str {
    if confirm_active {
        return "y/Enter confirm \u{00b7} n/Esc cancel";
    }
    if rename_active {
        return "type new title \u{00b7} Enter save \u{00b7} Esc cancel";
    }
    match state {
        PickerState::Loading => "Esc back",
        PickerState::Error { .. } => "r retry \u{00b7} Esc back",
        PickerState::Populated {
            search_focused: true,
            ..
        } => "type to filter \u{00b7} Enter commit \u{00b7} Esc exit search",
        PickerState::Populated {
            cursor: 0,
            search_focused: false,
            ..
        } => "j/k nav \u{00b7} Enter new session \u{00b7} / search \u{00b7} P preview \u{00b7} Esc back",
        PickerState::Populated { .. } => {
            "j/k nav \u{00b7} Enter resume \u{00b7} / search \u{00b7} n new \u{00b7} R rename \u{00b7} p pin \u{00b7} x archive \u{00b7} d deprecated \u{00b7} y yank-id \u{00b7} P preview \u{00b7} Esc back"
        }
    }
}

fn footer_hint_compact(
    state: &PickerState,
    rename_active: bool,
    confirm_active: bool,
) -> &'static str {
    if confirm_active {
        return "y/Enter confirm \u{00b7} n/Esc cancel";
    }
    if rename_active {
        return "type new title \u{00b7} Enter save \u{00b7} Esc cancel";
    }
    match state {
        PickerState::Loading => "Esc back",
        PickerState::Error { .. } => "r retry \u{00b7} Esc back",
        PickerState::Populated {
            search_focused: true,
            ..
        } => "\u{2191}\u{2193} pick \u{00b7} Esc",
        PickerState::Populated {
            cursor: 0,
            search_focused: false,
            ..
        } => "j/k nav \u{00b7} \u{21b5} new \u{00b7} / search \u{00b7} Esc",
        PickerState::Populated { .. } => {
            "j/k nav \u{00b7} \u{21b5} resume \u{00b7} / search \u{00b7} y yank \u{00b7} Esc"
        }
    }
}

fn render_footer_hint(frame: &mut Frame, area: Rect, hint: &str, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(Span::styled(
            hint,
            Style::default().fg(token(theme, "session_picker.footer_hint.fg")),
        )),
        area,
    );
}

fn compute_label_budget(area_width: u16, show_cwd: bool, show_brain: bool) -> usize {
    let mut gutter = 2 /* prefix */ + 8 + 2 /* short_id+gap */ + 8 + 2 /* time+gap */;
    if show_brain {
        gutter += 8 + 2;
    }
    if show_cwd {
        gutter += 16 + 2; // cwd basename + slash + gap
    }
    let avail = (area_width as usize).saturating_sub(gutter);
    avail.clamp(8, 60)
}

fn build_filter_haystack(
    session: &SessionInfo,
    metadata: &SessionMetadata,
    synopsis: &spur_core::SessionSynopsisProjection,
) -> String {
    let entry = metadata.sessions.get(session.session_id.0.as_ref());
    let synopsis_for = synopsis.get(&spur_acp::SessionId(
        session.session_id.0.as_ref().to_string(),
    ));
    let label = resolve_label(session, entry, synopsis_for.as_ref(), false, usize::MAX);
    let first = synopsis_for
        .as_ref()
        .and_then(|s| s.first_user_msg.as_deref())
        .unwrap_or("");
    let last = synopsis_for
        .as_ref()
        .and_then(|s| s.last_user_msg.as_deref())
        .unwrap_or("");
    let cwd = session.cwd.display().to_string();
    let id = session.session_id.0.as_ref();
    format!("{label} {first} {last} {cwd} {id}")
}

fn filtered_indices(
    sessions: &[SessionInfo],
    filter: &str,
    metadata: &SessionMetadata,
    synopsis: &spur_core::SessionSynopsisProjection,
    show_archived: bool,
) -> Vec<usize> {
    let candidates: Vec<usize> = (0..sessions.len())
        .filter(|&i| {
            let archived = metadata
                .sessions
                .get(sessions[i].session_id.0.as_ref())
                .map(|e| e.archived)
                .unwrap_or(false);
            if archived {
                show_archived
            } else {
                true
            }
        })
        .collect();

    if filter.is_empty() {
        let mut all = candidates;
        all.sort_by(|&a, &b| {
            let ea = metadata.sessions.get(sessions[a].session_id.0.as_ref());
            let eb = metadata.sessions.get(sessions[b].session_id.0.as_ref());
            let pa = ea.map(|e| e.pinned).unwrap_or(false);
            let pb = eb.map(|e| e.pinned).unwrap_or(false);
            match (pa, pb) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let ta = sessions[a].updated_at.as_deref().unwrap_or("");
                    let tb = sessions[b].updated_at.as_deref().unwrap_or("");
                    tb.cmp(ta)
                }
            }
        });
        return all;
    }

    use nucleo_matcher::{
        pattern::{CaseMatching, Normalization, Pattern},
        Matcher,
    };
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = candidates
        .into_iter()
        .filter_map(|i| {
            let haystack = build_filter_haystack(&sessions[i], metadata, synopsis);
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                &mut matcher,
            )?;
            Some((score, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, i)| i).collect()
}

// ─── State ────────────────────────────────────────────────────────────

enum PickerState {
    Loading,
    Populated {
        agent: String,
        sessions: Vec<SessionInfo>,
        cursor: usize,
        search_focused: bool,
        filter: String,
    },
    Error {
        message: String,
    },
}

// ─── View ─────────────────────────────────────────────────────────────

/// Active inline-rename session state (shown as a prompt in the bottom chunk).
struct RenameState {
    session_id: String,
    buffer: String,
    /// Display label in place when rename mode was entered. Used to construct tombstone inverse.
    original_title: String,
}

/// Pending switch-confirm target. `Some` on the view means the banner is up
/// and the next `y`/`Enter` commits the encoded action.
enum ConfirmSwitchTarget {
    Resume(String),
    NewSession,
}

pub struct SessionPickerView {
    state: PickerState,
    /// Interior-mutable so render(&self) can adjust scroll position.
    scroll_offset: Cell<usize>,
    metadata: SessionMetadata,
    /// View-level toggle; when false, archived sessions are hidden.
    show_archived: bool,
    /// `Some` when the user pressed `R`; intercepts all keys until
    /// `Enter` commits or `Esc` cancels.
    rename_state: Option<RenameState>,
    /// Toggled via uppercase `P`; when true, renders a metadata preview
    /// pane below the list.
    preview_visible: bool,
    /// ID of the current session if it has an unsent draft; None otherwise.
    /// Used to decide whether Enter on a different session (or [+ New])
    /// should show the switch-safety confirm banner.
    current_session_with_draft: Option<String>,
    /// SPUR session id of the currently-active SessionDetail, if any.
    /// Distinct from `current_session_with_draft` (which is Some only when
    /// the active session has UNSENT draft text). Used so that Enter on the
    /// current session's row short-circuits to NavigateTo instead of
    /// pointlessly re-resuming the session the user is already in.
    current_session_id: Option<String>,
    /// `Some` when the confirm-switch banner is up. Encodes what to do on
    /// `y`/`Enter` confirm: resume the given id, or start a new session.
    confirm_switch: Option<ConfirmSwitchTarget>,
    /// When set by landing, preselect this ACP session on first population
    /// and render a top banner. This never auto-fires Enter.
    preselect: Option<String>,
    preselect_consumed: bool,
}

impl Default for SessionPickerView {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPickerView {
    pub fn new() -> Self {
        Self {
            state: PickerState::Loading,
            scroll_offset: Cell::new(0),
            metadata: SessionMetadata::default(),
            show_archived: false,
            rename_state: None,
            preview_visible: false,
            current_session_with_draft: None,
            current_session_id: None,
            confirm_switch: None,
            preselect: None,
            preselect_consumed: false,
        }
    }

    pub fn with_preselect(preselect: Option<String>) -> Self {
        Self {
            preselect,
            ..Self::new()
        }
    }

    pub fn is_rename_active(&self) -> bool {
        self.rename_state.is_some()
    }

    pub(crate) fn is_search_focused(&self) -> bool {
        matches!(
            &self.state,
            PickerState::Populated {
                search_focused: true,
                ..
            }
        )
    }

    #[cfg(any(test, debug_assertions))]
    pub fn rename_buffer_for_test(&self) -> Option<&str> {
        self.rename_state
            .as_ref()
            .map(|state| state.buffer.as_str())
    }

    pub fn is_preview_visible(&self) -> bool {
        self.preview_visible
    }

    /// Called by App whenever metadata changes OR picker is opened. Passes in
    /// the id of the current session if it has an unsent draft, else None.
    pub fn set_current_session_has_draft(&mut self, session_id: Option<String>) {
        self.current_session_with_draft = session_id;
    }

    /// Push the SPUR session id of the currently-active SessionDetail into
    /// the picker. `None` when Dashboard is the active view.
    pub fn set_current_session_id(&mut self, session_id: Option<String>) {
        self.current_session_id = session_id;
    }

    pub fn is_confirm_switch_visible(&self) -> bool {
        self.confirm_switch.is_some()
    }

    pub fn set_metadata(&mut self, metadata: SessionMetadata) {
        self.metadata = metadata;
    }

    pub fn toggle_show_archived(&mut self, synopsis: &spur_core::SessionSynopsisProjection) {
        let prev_highlight = self.highlighted_session_id(synopsis);
        let prev_cursor_was_new = matches!(&self.state, PickerState::Populated { cursor: 0, .. });
        self.show_archived = !self.show_archived;
        if let PickerState::Populated {
            sessions,
            cursor,
            filter,
            ..
        } = &mut self.state
        {
            let indices = filtered_indices(
                sessions,
                filter,
                &self.metadata,
                synopsis,
                self.show_archived,
            );
            let new_cursor = Self::project_cursor(
                sessions,
                &indices,
                &self.metadata,
                prev_highlight.as_deref(),
                prev_cursor_was_new,
            );
            *cursor = new_cursor;
            // Only reset scroll when the cursor lands on [+ New]; preserved
            // session cursors keep their scroll context.
            if new_cursor == 0 {
                self.scroll_offset.set(0);
            }
        }
    }

    pub fn is_show_archived(&self) -> bool {
        self.show_archived
    }

    pub fn cursor(&self) -> usize {
        match &self.state {
            PickerState::Populated { cursor, .. } => *cursor,
            _ => 0,
        }
    }

    pub fn filter(&self) -> String {
        match &self.state {
            PickerState::Populated { filter, .. } => filter.clone(),
            _ => String::new(),
        }
    }

    pub fn set_sessions(
        &mut self,
        agent: String,
        sessions: Vec<SessionInfo>,
        synopsis: &spur_core::SessionSynopsisProjection,
    ) {
        // P2 (cursor preservation by session_id) — only meaningful when we
        // were already Populated. Captured here so it sees the *previous*
        // state before we overwrite it.
        let prev_highlight = self.highlighted_session_id(synopsis);
        let prev_cursor_was_new = matches!(&self.state, PickerState::Populated { cursor: 0, .. });
        let prev_filter = match &self.state {
            PickerState::Populated { filter, .. } => filter.clone(),
            _ => String::new(),
        };

        let indices = filtered_indices(
            &sessions,
            &prev_filter,
            &self.metadata,
            synopsis,
            self.show_archived,
        );

        let cursor = if !self.preselect_consumed {
            if let Some(target) = self.preselect.as_ref() {
                indices
                    .iter()
                    .position(|&i| sessions[i].session_id.0.as_ref() == target)
                    .map(|p| p + 1)
                    .unwrap_or(0)
            } else {
                Self::project_cursor(
                    &sessions,
                    &indices,
                    &self.metadata,
                    prev_highlight.as_deref(),
                    prev_cursor_was_new,
                )
            }
        } else {
            Self::project_cursor(
                &sessions,
                &indices,
                &self.metadata,
                prev_highlight.as_deref(),
                prev_cursor_was_new,
            )
        };
        self.preselect_consumed = self.preselect.is_some();

        self.state = PickerState::Populated {
            agent,
            sessions,
            cursor,
            search_focused: false,
            filter: prev_filter,
        };
        self.scroll_offset.set(0);
    }

    fn highlighted_session_id(
        &self,
        synopsis: &spur_core::SessionSynopsisProjection,
    ) -> Option<String> {
        let PickerState::Populated {
            sessions,
            cursor,
            filter,
            ..
        } = &self.state
        else {
            return None;
        };
        if *cursor == 0 {
            return None;
        }
        let indices = filtered_indices(
            sessions,
            filter,
            &self.metadata,
            synopsis,
            self.show_archived,
        );
        let real_idx = indices.get(*cursor - 1).copied()?;
        Some(sessions[real_idx].session_id.0.as_ref().to_string())
    }

    fn project_cursor(
        sessions: &[SessionInfo],
        indices: &[usize],
        metadata: &SessionMetadata,
        prev_highlight: Option<&str>,
        prev_cursor_was_new: bool,
    ) -> usize {
        if prev_cursor_was_new {
            return 0;
        }
        if let Some(id) = prev_highlight {
            if let Some(p) = indices
                .iter()
                .position(|&i| sessions[i].session_id.0.as_ref() == id)
            {
                return p + 1;
            }
        }
        if let Some(id) = metadata.last_active_session_id.as_deref() {
            if let Some(p) = indices
                .iter()
                .position(|&i| sessions[i].session_id.0.as_ref() == id)
            {
                return p + 1;
            }
        }
        if !indices.is_empty() {
            1
        } else {
            0
        }
    }

    pub fn visible_session_count(&self, synopsis: &spur_core::SessionSynopsisProjection) -> usize {
        match &self.state {
            PickerState::Populated {
                sessions, filter, ..
            } => filtered_indices(
                sessions,
                filter,
                &self.metadata,
                synopsis,
                self.show_archived,
            )
            .len(),
            _ => 0,
        }
    }

    pub fn visible_session_at(
        &self,
        idx: usize,
        synopsis: &spur_core::SessionSynopsisProjection,
    ) -> Option<&SessionInfo> {
        match &self.state {
            PickerState::Populated {
                sessions, filter, ..
            } => filtered_indices(
                sessions,
                filter,
                &self.metadata,
                synopsis,
                self.show_archived,
            )
            .get(idx)
            .and_then(|&i| sessions.get(i)),
            _ => None,
        }
    }

    pub fn set_error(&mut self, message: String) {
        self.state = PickerState::Error { message };
    }

    pub fn handle_paste(&mut self, text: &str) {
        let first_line = text.lines().next().unwrap_or("");
        if first_line.is_empty() {
            return;
        }
        if let Some(ref mut rs) = self.rename_state {
            rs.buffer.push_str(first_line);
            return;
        }
        if let PickerState::Populated {
            search_focused: true,
            filter,
            cursor,
            ..
        } = &mut self.state
        {
            filter.push_str(first_line);
            *cursor = 0;
        }
    }

    fn relative_time(iso: &str) -> String {
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
            return String::new();
        };
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        let secs = diff.num_seconds();
        if secs < 60 {
            "just now".to_string()
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 86400 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }

    fn cwds_are_heterogeneous(sessions: &[SessionInfo]) -> bool {
        if sessions.len() <= 1 {
            return false;
        }
        let first = &sessions[0].cwd;
        sessions.iter().any(|s| s.cwd != *first)
    }

    fn brains_are_heterogeneous(sessions: &[SessionInfo], metadata: &SessionMetadata) -> bool {
        if sessions.len() <= 1 {
            return false;
        }
        let first = metadata
            .sessions
            .get(sessions[0].session_id.0.as_ref())
            .and_then(|e| e.brain_name.as_deref());
        sessions.iter().any(|s| {
            let b = metadata
                .sessions
                .get(s.session_id.0.as_ref())
                .and_then(|e| e.brain_name.as_deref());
            b != first
        })
    }

    fn cwd_basename(cwd: &std::path::Path) -> &str {
        cwd.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| cwd.to_str().unwrap_or(""))
    }

    fn render_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        flag_summary: Option<(usize, usize)>,
        tombstone: Option<&Tombstone>,
        view_hint_override: Option<HintOverride<'_>>,
    ) {
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default()
                    .fg(token(theme, "session_picker.title.fg"))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  Connecting to agent"),
                Span::styled(
                    " \u{00b7}\u{00b7}\u{00b7}",
                    Style::default().fg(token(theme, "session_picker.spinner.fg")),
                ),
            ]),
        ];
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
        let v_pad = chunks[0].height.saturating_sub(4) / 3;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };
        frame.render_widget(Paragraph::new(lines), content_area);
        StatusBar::render(
            frame,
            chunks[1],
            StatusBarProps {
                view: &ViewId::SessionPicker,
                theme,
                tombstone,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count: 0,
                alert_summary: None,
                license_badge,
                flag_summary,
                view_hint_override: view_hint_override.or(Some(HintOverride {
                    full: footer_hint(&self.state, false, false),
                    compact: Some(footer_hint_compact(&self.state, false, false)),
                    hide_on_overflow: true,
                })),
            },
        );
    }

    fn build_preselect_banner(
        &self,
        acp_id: &str,
        synopsis: &spur_core::SessionSynopsisProjection,
        theme: &Theme,
    ) -> Line<'static> {
        if let PickerState::Populated { sessions, .. } = &self.state {
            if let Some(session) = sessions.iter().find(|s| s.session_id.0.as_ref() == acp_id) {
                let synopsis_key = spur_acp::SessionId(session.session_id.0.as_ref().to_string());
                let synopsis_data = synopsis.get(&synopsis_key);
                let label = resolve_label(
                    session,
                    self.metadata.sessions.get(session.session_id.0.as_ref()),
                    synopsis_data.as_ref(),
                    false,
                    usize::MAX,
                );
                let mut spans = vec![
                    Span::raw(" Last: "),
                    Span::styled(
                        label,
                        Style::default()
                            .fg(token(theme, "session_picker.banner.label.fg"))
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if let Some(relative) = session
                    .updated_at
                    .as_deref()
                    .map(Self::relative_time)
                    .filter(|relative| !relative.is_empty())
                {
                    spans.push(Span::raw("  ·  "));
                    spans.push(Span::styled(
                        relative,
                        Style::default().fg(token(theme, "session_picker.banner.timestamp.fg")),
                    ));
                }
                spans.extend([
                    Span::raw("  ·  "),
                    Span::styled(
                        "[Enter] resume",
                        Style::default().fg(token(theme, "session_picker.banner.action.fg")),
                    ),
                    Span::raw("  ·  "),
                    Span::styled(
                        "[n] new",
                        Style::default().fg(token(theme, "session_picker.banner.muted.fg")),
                    ),
                ]);
                return Line::from(spans);
            }

            let short = acp_id[..8.min(acp_id.len())].to_string();
            return Line::from(vec![
                Span::styled(
                    " Session ",
                    Style::default().fg(token(theme, "session_picker.banner.error.fg")),
                ),
                Span::styled(
                    short,
                    Style::default().fg(token(theme, "session_picker.banner.error_id.fg")),
                ),
                Span::styled(
                    " not found  ·  ",
                    Style::default().fg(token(theme, "session_picker.banner.error.fg")),
                ),
                Span::styled(
                    "[Enter] new",
                    Style::default().fg(token(theme, "session_picker.banner.action.fg")),
                ),
                Span::raw("  ·  "),
                Span::styled(
                    "[Esc] cancel",
                    Style::default().fg(token(theme, "session_picker.banner.muted.fg")),
                ),
            ]);
        }

        Line::from(format!(" Loading session list for {acp_id}..."))
    }

    // Refactoring to a props struct is deferred — the signature is stable and
    // every caller already passes every arg. See `StatusBarProps` for the
    // pattern if/when we do fold these.
    #[allow(clippy::too_many_arguments)]
    fn render_populated(
        &self,
        frame: &mut Frame,
        area: Rect,
        ctx: &super::ViewContext,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        flag_summary: Option<(usize, usize)>,
        agent: &str,
        sessions: &[SessionInfo],
        cursor: usize,
        search_focused: bool,
        filter: &str,
    ) {
        let show_cwd = Self::cwds_are_heterogeneous(sessions);
        let visible_height = area.height.saturating_sub(4) as usize;

        let indices = filtered_indices(
            sessions,
            filter,
            &self.metadata,
            ctx.synopsis,
            self.show_archived,
        );

        // Clamp scroll_offset so cursor is always visible.
        // `cursor` indexes a virtual list where 0 = [+ New session] row and
        // 1..=filtered.len() are real (visible) sessions. We scroll the real
        // sessions; the [+ New session] row is always visible as the first entry.
        let mut scroll = self.scroll_offset.get();
        let session_cursor = cursor.saturating_sub(1);
        if cursor >= 1 && session_cursor >= scroll + visible_height {
            scroll = session_cursor.saturating_sub(visible_height.saturating_sub(1));
        }
        if cursor >= 1 && session_cursor < scroll {
            scroll = session_cursor;
        }
        self.scroll_offset.set(scroll);

        let mut header_spans = vec![
            Span::styled(
                "Sessions ",
                Style::default()
                    .fg(token(ctx.theme, "session_picker.title.fg"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({})", agent),
                Style::default().fg(token(ctx.theme, "session_picker.row.muted.fg")),
            ),
        ];
        if self.show_archived {
            header_spans.push(Span::styled(
                " [showing archived]",
                Style::default().fg(token(ctx.theme, "session_picker.row.muted.fg")),
            ));
        }
        let mut lines = vec![
            Line::from(header_spans),
            Line::from(vec![
                Span::styled(
                    "  Search  ",
                    Style::default().fg(token(ctx.theme, "session_picker.search.label.fg")),
                ),
                Span::styled(
                    format!("{}{}", filter, if search_focused { "_" } else { "" }),
                    Style::default().fg(if search_focused {
                        token(ctx.theme, "session_picker.search.active.fg")
                    } else {
                        token(ctx.theme, "session_picker.search.inactive.fg")
                    }),
                ),
            ]),
            Line::from(""),
        ];

        // [+ Start new session] virtual row.
        let is_new_selected = cursor == 0;
        let new_prefix = if is_new_selected { "\u{25b8} " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                new_prefix,
                if is_new_selected {
                    Style::default().fg(token(ctx.theme, "session_picker.new_row.fg"))
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                "+ Start new session",
                if is_new_selected {
                    Style::default()
                        .fg(token(ctx.theme, "session_picker.new_row.fg"))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(token(ctx.theme, "session_picker.new_row.fg"))
                },
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "  \u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(token(ctx.theme, "session_picker.row.separator.fg")),
        )));

        let show_brain = Self::brains_are_heterogeneous(sessions, &self.metadata);

        for (display_i, real_i) in indices.iter().enumerate().skip(scroll).take(visible_height) {
            let session = &sessions[*real_i];
            let is_selected = cursor == display_i + 1;
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let raw_id = session.session_id.0.as_ref();
            let short_id = &raw_id[..8.min(raw_id.len())];
            let synopsis_key = spur_acp::SessionId(session.session_id.0.as_ref().to_string());
            let synopsis = ctx.synopsis.get(&synopsis_key);
            let label_budget = compute_label_budget(area.width, show_cwd, show_brain);
            let display = resolve_label(
                session,
                self.metadata.sessions.get(session.session_id.0.as_ref()),
                synopsis.as_ref(),
                show_cwd,
                label_budget,
            );
            let time_str = session
                .updated_at
                .as_deref()
                .map(Self::relative_time)
                .unwrap_or_default();

            let cwd_suffix = if show_cwd {
                format!("  {}/", Self::cwd_basename(&session.cwd))
            } else {
                String::new()
            };

            let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
            let archived = entry.map(|e| e.archived).unwrap_or(false);
            let pinned = entry.map(|e| e.pinned).unwrap_or(false);
            let brain = entry.and_then(|e| e.brain_name.as_deref()).unwrap_or("");

            let title_style = if archived {
                Style::default().fg(token(ctx.theme, "session_picker.row.archived.fg"))
            } else if is_selected {
                Style::default()
                    .fg(token(ctx.theme, "session_picker.row.title_selected.fg"))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let muted_style = Style::default().fg(token(ctx.theme, "session_picker.row.muted.fg"));

            let mut spans: Vec<Span> = Vec::with_capacity(10);
            spans.push(Span::styled(
                prefix,
                if is_selected {
                    Style::default().fg(token(ctx.theme, "session_picker.row.cursor.fg"))
                } else {
                    Style::default()
                },
            ));
            if pinned {
                spans.push(Span::styled(
                    "\u{2b50} ",
                    Style::default().fg(token(ctx.theme, "session_picker.row.pinned.fg")),
                ));
            }
            spans.push(Span::styled(display, title_style));
            spans.push(Span::styled(cwd_suffix, muted_style));
            if show_brain {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(brain, muted_style));
            }
            spans.push(Span::raw("  "));
            spans.push(Span::styled(time_str, muted_style));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(short_id, muted_style));
            if archived {
                spans.push(Span::styled(" [archived]", muted_style));
            }
            lines.push(Line::from(spans));
        }

        // Layout: chunks[1]/[2] (status + footer hint) are kept as two rows
        // ONLY when a state-specific prompt is active (rename or confirm-switch),
        // so that the prompt and its key-hint can coexist on separate rows.
        // In normal/list mode, the StatusBar's `view_hint_override` already
        // carries the key hints alongside the stats, so a separate footer row
        // would duplicate. We collapse to one row in that case.
        let needs_footer_row = self.rename_state.is_some() || self.confirm_switch.is_some();
        let chunks = if self.preview_visible {
            if needs_footer_row {
                Layout::vertical([
                    Constraint::Min(4),
                    Constraint::Length(PREVIEW_MAX_LINES),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(area)
            } else {
                Layout::vertical([
                    Constraint::Min(4),
                    Constraint::Length(PREVIEW_MAX_LINES),
                    Constraint::Length(1),
                ])
                .split(area)
            }
        } else if needs_footer_row {
            Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area)
        } else {
            Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area)
        };
        frame.render_widget(Paragraph::new(lines), chunks[0]);

        // Status row (chunks[status_idx]) and optional footer-hint row (chunks[footer_idx]).
        // Footer-hint row is only allocated when a state-specific prompt is active.
        let (status_idx, footer_idx_opt) = if self.preview_visible {
            (2, if needs_footer_row { Some(3) } else { None })
        } else {
            (1, if needs_footer_row { Some(2) } else { None })
        };

        if self.preview_visible {
            use crate::components::session_preview::{PreviewContent, PreviewRow, SessionPreview};
            use ratatui::style::Style;

            let content = if cursor == 0 {
                PreviewContent {
                    rows: vec![],
                    placeholder: Some(
                        "Press Enter to start a new session \u{00b7} any unsent draft will be saved"
                            .to_string(),
                    ),
                }
            } else {
                let indices = filtered_indices(
                    sessions,
                    filter,
                    &self.metadata,
                    ctx.synopsis,
                    self.show_archived,
                );
                let real_idx = indices.get(cursor - 1).copied();
                if let Some(i) = real_idx {
                    let session = &sessions[i];
                    let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
                    // Use the same SessionId wrapper conversion idiom as Task 15.
                    let synopsis_key =
                        spur_acp::SessionId(session.session_id.0.as_ref().to_string());
                    let synopsis = ctx.synopsis.get(&synopsis_key);
                    let draft = entry.map(|e| e.draft.clone()).unwrap_or_default();
                    let brain = entry.and_then(|e| e.brain_name.clone()).unwrap_or_default();
                    let cwd = session.cwd.display().to_string();
                    let short_id = {
                        let raw = session.session_id.0.as_ref();
                        raw[..8.min(raw.len())].to_string()
                    };
                    let value_width = (chunks[1].width as usize)
                        .saturating_sub("  Intent: ".len())
                        .max(1);
                    let footer_width = (chunks[1].width as usize).saturating_sub(2).max(1);

                    let mut rows: Vec<PreviewRow> = Vec::new();

                    // 1. Last user message (state-first: what was just said)
                    if let Some(last) = synopsis.as_ref().and_then(|s| s.last_user_msg.clone()) {
                        rows.push(PreviewRow {
                            label: "Last".into(),
                            value_lines: vec![truncate_for_row(&last, value_width)],
                            value_style: None,
                        });
                    }

                    // 2. Draft (state-first: what's pending)
                    if !draft.is_empty() {
                        rows.push(PreviewRow {
                            label: "Draft".into(),
                            value_lines: vec![truncate_for_row(&draft, value_width)],
                            value_style: Some(
                                Style::default()
                                    .fg(token(ctx.theme, "session_picker.preview.draft.fg")),
                            ),
                        });
                    }

                    // 3. Blank separator between state-first and original-intent
                    rows.push(PreviewRow::default());

                    // 4. Intent (original first message, dim/wrapped)
                    if let Some(first) = synopsis.as_ref().and_then(|s| s.first_user_msg.clone()) {
                        rows.push(PreviewRow {
                            label: "Intent".into(),
                            value_lines: wrap_value(&first, value_width, 3),
                            value_style: Some(
                                Style::default()
                                    .fg(token(ctx.theme, "session_picker.preview.intent.fg")),
                            ),
                        });
                    } else {
                        rows.push(PreviewRow {
                            label: String::new(),
                            value_lines: vec![truncate_for_row(
                                "(resume to load message history)",
                                footer_width,
                            )],
                            value_style: Some(
                                Style::default()
                                    .fg(token(ctx.theme, "session_picker.preview.placeholder.fg"))
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        });
                    }

                    // 5. Blank separator
                    rows.push(PreviewRow::default());

                    // 6. Footer (cwd · brain · short id)
                    rows.push(PreviewRow {
                        label: "".into(),
                        value_lines: vec![truncate_for_row(
                            &format!("{cwd} \u{00b7} {brain} \u{00b7} {short_id}"),
                            footer_width,
                        )],
                        value_style: Some(
                            Style::default()
                                .fg(token(ctx.theme, "session_picker.preview.footer.fg")),
                        ),
                    });

                    // Bounded by construction: Last <= 1, Draft <= 1, then
                    // either Intent <= 3 or placeholder <= 1, footer <= 1,
                    // plus two blank separators = <= 8 visual lines, leaving
                    // slack under PREVIEW_MAX_LINES for the preview border.

                    PreviewContent {
                        rows,
                        placeholder: None,
                    }
                } else {
                    PreviewContent::default()
                }
            };
            SessionPreview::render(frame, chunks[1], &content);
        }

        if let Some(ref target) = self.confirm_switch {
            let current = self
                .current_session_with_draft
                .as_deref()
                .unwrap_or("current session");
            let action_desc = match target {
                ConfirmSwitchTarget::Resume(id) => format!("resume {}", id),
                ConfirmSwitchTarget::NewSession => "start a new session".to_string(),
            };
            let prompt = format!(
                "Session \"{current}\" has an unsent draft — save and {action_desc}? [y/N]"
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    prompt,
                    Style::default()
                        .fg(token(ctx.theme, "session_picker.prompt.confirm.fg"))
                        .add_modifier(Modifier::BOLD),
                )),
                chunks[status_idx],
            );
        } else if let Some(ref rs) = self.rename_state {
            let prompt = format!("Rename \u{2192} {}_", rs.buffer);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    prompt,
                    Style::default()
                        .fg(token(ctx.theme, "session_picker.prompt.rename.fg"))
                        .add_modifier(Modifier::BOLD),
                )),
                chunks[status_idx],
            );
        } else {
            StatusBar::render(
                frame,
                chunks[status_idx],
                StatusBarProps {
                    view: &ViewId::SessionPicker,
                    theme: ctx.theme,
                    tombstone: ctx.tombstone,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0m 00s",
                    current_mode: None,
                    current_model_label: None,
                    current_effort_label: None,
                    usage_supported: false,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    esc_consumed_by_composer: false,
                    issue_count: 0,
                    alert_summary: None,
                    license_badge,
                    flag_summary,
                    view_hint_override: ctx.transient_hint_override.or(Some(HintOverride {
                        full: footer_hint(
                            &self.state,
                            self.rename_state.is_some(),
                            self.confirm_switch.is_some(),
                        ),
                        compact: Some(footer_hint_compact(
                            &self.state,
                            self.rename_state.is_some(),
                            self.confirm_switch.is_some(),
                        )),
                        hide_on_overflow: true,
                    })),
                },
            );
        }
        // Render the contextual key-hint row ONLY when a state-specific prompt
        // occupies the status row. In normal/list mode, the StatusBar already
        // carries the hint via its view_hint_override (avoiding duplication).
        if let Some(footer_idx) = footer_idx_opt {
            let hint = footer_hint(
                &self.state,
                self.rename_state.is_some(),
                self.confirm_switch.is_some(),
            );
            render_footer_hint(frame, chunks[footer_idx], hint, ctx.theme);
        }
    }

    fn render_error(&self, frame: &mut Frame, area: Rect, message: &str, ctx: &super::ViewContext) {
        let license_badge = ctx.license_badge;
        let flag_summary = ctx.flag_summary;
        let tombstone = ctx.tombstone;
        let view_hint_override = ctx.transient_hint_override;
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default()
                    .fg(token(ctx.theme, "session_picker.error.title.fg"))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", message),
                Style::default().fg(token(ctx.theme, "session_picker.error.message.fg")),
            )),
            Line::from(Span::styled(
                "  Use --resume <id> to load a session by ID.",
                Style::default().fg(token(ctx.theme, "session_picker.error.hint.fg")),
            )),
        ];
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
        let v_pad = chunks[0].height.saturating_sub(5) / 3;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };
        frame.render_widget(Paragraph::new(lines), content_area);
        StatusBar::render(
            frame,
            chunks[1],
            StatusBarProps {
                view: &ViewId::SessionPicker,
                theme: ctx.theme,
                tombstone,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count: 0,
                alert_summary: None,
                license_badge,
                flag_summary,
                view_hint_override: view_hint_override.or(Some(HintOverride {
                    full: footer_hint(&self.state, false, false),
                    compact: Some(footer_hint_compact(&self.state, false, false)),
                    hide_on_overflow: true,
                })),
            },
        );
    }

    #[cfg(test)]
    pub(super) fn debug_cursor(&self) -> Option<usize> {
        match &self.state {
            PickerState::Populated { cursor, .. } => Some(*cursor),
            _ => None,
        }
    }
}

/// Truncate a string for row display: cut at the first sentence
/// boundary or `budget` graphemes, whichever comes first. Strips
/// leading whitespace. Adds `…` when the cut shortened the text or
/// when the budget is < 1.
pub(super) fn truncate_for_row(input: &str, budget: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    let trimmed = input.trim_start();
    if budget == 0 {
        return "\u{2026}".to_string();
    }

    let punct_cut = trimmed.find(['.', '?', '!', '\n']);
    let punct_text = punct_cut.map(|i| &trimmed[..i]).unwrap_or(trimmed);

    let graphemes: Vec<&str> = punct_text.graphemes(true).collect();
    if graphemes.len() <= budget && punct_cut.is_none() {
        return punct_text.to_string();
    }
    if graphemes.len() <= budget {
        return punct_text.to_string();
    }
    let mut out: String = graphemes.iter().take(budget).copied().collect();
    out.push('\u{2026}');
    out
}

pub(super) fn wrap_value(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    if max_lines == 0 {
        return Vec::new();
    }
    if width == 0 {
        return vec!["\u{2026}".to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut graphemes = text.graphemes(true).peekable();
    let mut truncated = false;

    while let Some(g) = graphemes.next() {
        if matches!(g, "\n" | "\r\n" | "\r") {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
            if lines.len() == max_lines {
                truncated = graphemes.peek().is_some();
                break;
            }
            continue;
        }

        let grapheme_width = UnicodeWidthStr::width(g).max(1);
        if grapheme_width > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                if lines.len() == max_lines {
                    truncated = true;
                    break;
                }
            }
            lines.push(g.to_string());
            current_width = 0;
            if lines.len() == max_lines {
                truncated = graphemes.peek().is_some();
                break;
            }
            continue;
        }

        if current_width > 0 && current_width + grapheme_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
            if lines.len() == max_lines {
                truncated = true;
                break;
            }
        }

        current.push_str(g);
        current_width += grapheme_width;
    }

    let ended_with_line_break = text.ends_with('\n') || text.ends_with('\r');
    if !truncated
        && lines.len() < max_lines
        && (lines.is_empty() || !current.is_empty() || ended_with_line_break)
    {
        lines.push(current);
    }

    if truncated {
        if lines.is_empty() {
            lines.push(String::new());
        }
        let last = lines.last_mut().expect("truncated output has a last line");
        append_ellipsis(last, width);
    }

    lines
}

fn append_ellipsis(line: &mut String, width: usize) {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    while !line.is_empty() && UnicodeWidthStr::width(line.as_str()) + 1 > width {
        let keep_bytes = line
            .grapheme_indices(true)
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        line.truncate(keep_bytes);
    }
    line.push('\u{2026}');
}

/// Resolve which label to render for a session-picker row. Precedence:
/// title_override > first_user_msg > agent title > cwd basename >
/// "(untitled session)". Empty strings are skipped at each tier.
pub(super) fn resolve_label(
    session: &spur_acp::SessionInfo,
    entry: Option<&crate::session_metadata::SessionEntry>,
    synopsis: Option<&spur_core::SessionSynopsis>,
    show_cwd: bool,
    label_budget: usize,
) -> String {
    if let Some(t) = entry
        .and_then(|e| e.title_override.as_deref())
        .filter(|t| !t.is_empty())
    {
        return truncate_for_row(t, label_budget);
    }
    if let Some(snippet) = synopsis
        .and_then(|s| s.first_user_msg.as_deref())
        .filter(|s| !s.is_empty())
    {
        return truncate_for_row(snippet, label_budget);
    }
    if let Some(t) = session.title.as_deref().filter(|t| !t.is_empty()) {
        return truncate_for_row(t, label_budget);
    }
    if show_cwd {
        return format!("{}/", SessionPickerView::cwd_basename(&session.cwd));
    }
    "(untitled session)".to_string()
}

impl View for SessionPickerView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        // 0. Confirm-switch intercepts all keys until y/Enter commits or cancel keys dismiss.
        if let Some(ref target) = self.confirm_switch {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let out = match target {
                        ConfirmSwitchTarget::Resume(id) => Action::ResumeSession {
                            session_id: id.clone(),
                        },
                        ConfirmSwitchTarget::NewSession => Action::NewSessionRequested,
                    };
                    self.confirm_switch = None;
                    return Some(out);
                }
                KeyCode::Char('u') if key.modifiers.is_empty() => return None,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_switch = None;
                    return None;
                }
                _ => {
                    self.confirm_switch = None;
                    return None;
                }
            }
        }

        // 1. Rename-mode intercepts all keys until Enter/Esc.
        if let Some(rs) = self.rename_state.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let out = Action::RenameSession {
                        session_id: rs.session_id.clone(),
                        new_title: rs.buffer.clone(),
                        original_title: rs.original_title.clone(),
                    };
                    self.rename_state = None;
                    return Some(out);
                }
                KeyCode::Esc => {
                    self.rename_state = None;
                    return None;
                }
                KeyCode::Backspace => {
                    rs.buffer.pop();
                    return None;
                }
                KeyCode::Char(c) => {
                    rs.buffer.push(c);
                    return None;
                }
                _ => return None,
            }
        }

        // Preview toggle intercepts before list-mode logic. Only valid when
        // search isn't focused (so capital P typed in search box still filters).
        let can_toggle_preview = matches!(
            &self.state,
            PickerState::Populated {
                search_focused: false,
                ..
            }
        );
        if can_toggle_preview {
            if let KeyCode::Char('P') = key.code {
                self.preview_visible = !self.preview_visible;
                return None;
            }
        }

        // Compute once — needed by list-mode p/R/d arms before we split-borrow.
        let hl_session_id = self.highlighted_session_id(ctx.synopsis);

        // Deferred state transitions to apply after the split borrow ends.
        enum Post {
            None,
            StartRename {
                session_id: String,
                buffer: String,
                original_title: String,
            },
            StartConfirmSwitch(ConfirmSwitchTarget),
        }
        let mut post = Post::None;

        // Split-borrow self so we can reach `metadata` while also mutably
        // borrowing `state`.
        let action = {
            let SessionPickerView {
                state,
                metadata,
                show_archived,
                current_session_with_draft,
                current_session_id,
                ..
            } = self;
            match state {
                PickerState::Populated {
                    sessions,
                    cursor,
                    search_focused,
                    filter,
                    ..
                } => {
                    if *search_focused {
                        match key.code {
                            KeyCode::Esc => {
                                *search_focused = false;
                                None
                            }
                            KeyCode::Enter => {
                                *search_focused = false;
                                None
                            }
                            KeyCode::Backspace => {
                                filter.pop();
                                *cursor = 0;
                                None
                            }
                            KeyCode::Char(c) => {
                                filter.push(c);
                                *cursor = 0;
                                None
                            }
                            _ => None,
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('/') => {
                                *search_focused = true;
                                None
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if *cursor > 0 {
                                    *cursor -= 1;
                                }
                                None
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let visible = filtered_indices(
                                    sessions,
                                    filter,
                                    metadata,
                                    ctx.synopsis,
                                    *show_archived,
                                )
                                .len();
                                if *cursor < visible {
                                    *cursor += 1;
                                }
                                None
                            }
                            KeyCode::Char('n') => {
                                if current_session_with_draft.is_some() {
                                    post =
                                        Post::StartConfirmSwitch(ConfirmSwitchTarget::NewSession);
                                    None
                                } else {
                                    Some(Action::NewSessionRequested)
                                }
                            }
                            KeyCode::Enter => {
                                if *cursor == 0 {
                                    // [+ New session] row: if the current session has a
                                    // draft, ask the user to confirm switching away.
                                    if current_session_with_draft.is_some() {
                                        post = Post::StartConfirmSwitch(
                                            ConfirmSwitchTarget::NewSession,
                                        );
                                        None
                                    } else {
                                        Some(Action::NewSessionRequested)
                                    }
                                } else {
                                    let indices = filtered_indices(
                                        sessions,
                                        filter,
                                        metadata,
                                        ctx.synopsis,
                                        *show_archived,
                                    );
                                    let real_idx = indices.get(*cursor - 1).copied()?;
                                    let sid = sessions[real_idx].session_id.0.to_string();

                                    if current_session_id.as_deref() == Some(sid.as_str()) {
                                        // Short-circuit: the selected row IS the currently-active session.
                                        // Don't re-resume — just navigate back to its detail view. No
                                        // backend traffic; no confirm-switch banner (there's nothing to
                                        // switch away from).
                                        Some(Action::NavigateTo(ViewId::SessionDetail(
                                            spur_acp::SessionId(sid),
                                        )))
                                    } else {
                                        // Confirm only when the draft belongs to a DIFFERENT session than
                                        // the one being resumed.
                                        let draft_elsewhere = current_session_with_draft
                                            .as_ref()
                                            .map(|cur| cur != &sid)
                                            .unwrap_or(false);
                                        if draft_elsewhere {
                                            post = Post::StartConfirmSwitch(
                                                ConfirmSwitchTarget::Resume(sid),
                                            );
                                            None
                                        } else {
                                            Some(Action::ResumeSession { session_id: sid })
                                        }
                                    }
                                }
                            }
                            KeyCode::Esc => {
                                if !filter.is_empty() {
                                    filter.clear();
                                    *cursor = 0;
                                    None
                                } else {
                                    Some(Action::NavigateTo(ViewId::Dashboard))
                                }
                            }
                            KeyCode::Char('p') => hl_session_id
                                .clone()
                                .map(|session_id| Action::ToggleSessionPin { session_id }),
                            KeyCode::Char('x') => hl_session_id.clone().map(|session_id| {
                                Action::ToggleSessionArchive {
                                    session_id,
                                    via_legacy_key: false,
                                }
                            }),
                            KeyCode::Char('d') => hl_session_id.clone().map(|session_id| {
                                Action::ToggleSessionArchive {
                                    session_id,
                                    via_legacy_key: true,
                                }
                            }),
                            KeyCode::Char('a') => Some(Action::ToggleShowArchived),
                            KeyCode::Char('r') => Some(Action::RefreshSessions),
                            KeyCode::Char('R') => {
                                if let Some(ref sid) = hl_session_id {
                                    let buffer = sessions
                                        .iter()
                                        .find(|s| s.session_id.0.as_ref() == sid.as_str())
                                        .map(|s| {
                                            let synopsis_key = spur_acp::SessionId(
                                                s.session_id.0.as_ref().to_string(),
                                            );
                                            let synopsis = ctx.synopsis.get(&synopsis_key);
                                            resolve_label(
                                                s,
                                                metadata.sessions.get(s.session_id.0.as_ref()),
                                                synopsis.as_ref(),
                                                false,
                                                usize::MAX,
                                            )
                                        })
                                        .unwrap_or_default();
                                    post = Post::StartRename {
                                        session_id: sid.clone(),
                                        original_title: buffer.clone(),
                                        buffer,
                                    };
                                }
                                None
                            }
                            KeyCode::Char('y') => hl_session_id.clone().map(Action::CopySessionId),
                            _ => None,
                        }
                    }
                }
                PickerState::Loading => match key.code {
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
                },
                PickerState::Error { .. } => match key.code {
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    KeyCode::Char('r') | KeyCode::Enter => Some(Action::RefreshSessions),
                    _ => None,
                },
            }
        };

        // Apply deferred state transitions.
        match post {
            Post::None => {}
            Post::StartRename {
                session_id,
                buffer,
                original_title,
            } => {
                self.rename_state = Some(RenameState {
                    session_id,
                    buffer,
                    original_title,
                });
            }
            Post::StartConfirmSwitch(target) => {
                self.confirm_switch = Some(target);
            }
        }

        action
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {
        // Picker holds no async state (see Tranche 2 Task 3). Events arrive
        // for completeness of the view dispatch architecture, but no
        // immediate action is taken — list refresh is driven by explicit
        // user intent or navigation. SessionsListed and SessionsListError
        // are handled by App, which calls set_sessions() or set_error()
        // directly.
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        let (banner_area, content_area) = if let Some(acp_id) = self.preselect.as_deref() {
            let [banner, content] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            frame.render_widget(
                Paragraph::new(self.build_preselect_banner(acp_id, ctx.synopsis, ctx.theme)),
                banner,
            );
            (Some(banner), content)
        } else {
            (None, area)
        };
        let _ = banner_area;
        match &self.state {
            PickerState::Loading => self.render_loading(
                frame,
                content_area,
                ctx.theme,
                ctx.license_badge,
                ctx.flag_summary,
                ctx.tombstone,
                ctx.transient_hint_override,
            ),
            PickerState::Populated {
                agent,
                sessions,
                cursor,
                search_focused,
                filter,
            } => self.render_populated(
                frame,
                content_area,
                ctx,
                ctx.license_badge,
                ctx.flag_summary,
                agent,
                sessions,
                *cursor,
                *search_focused,
                filter,
            ),
            PickerState::Error { message } => self.render_error(frame, content_area, message, ctx),
        }
    }

    fn tick(&mut self) {
        // No animations in the picker.
    }
}

#[cfg(test)]
mod current_session_shortcut_tests {
    use super::*;
    use crate::action::{Action, ViewId};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn make_session(id: &str) -> SessionInfo {
        SessionInfo::new(id.to_string(), PathBuf::from("/tmp"))
    }

    #[test]
    fn enter_on_current_session_row_navigates_back() {
        let mut picker = SessionPickerView::new();
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("A")],
            test_ctx().synopsis,
        );
        picker.set_current_session_id(Some("A".into()));

        // Cursor starts at 0 ([+ New session]); move to 1 (the A row).
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );

        let action = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &test_ctx(),
        );
        match action {
            Some(Action::NavigateTo(ViewId::SessionDetail(sid))) => {
                assert_eq!(sid.0, "A");
            }
            other => panic!("expected NavigateTo(SessionDetail(A)), got {:?}", other),
        }
    }

    #[test]
    fn enter_on_different_session_row_still_resumes() {
        let mut picker = SessionPickerView::new();
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("A"), make_session("B")],
            test_ctx().synopsis,
        );
        picker.set_current_session_id(Some("A".into()));

        // Move cursor to row index 2 = session B.
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );

        let action = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &test_ctx(),
        );
        match action {
            Some(Action::ResumeSession { session_id }) => {
                assert_eq!(session_id, "B");
            }
            other => panic!("expected ResumeSession(B), got {:?}", other),
        }
    }

    #[test]
    fn preselect_jumps_cursor_to_matching_session() {
        let mut picker = SessionPickerView::with_preselect(Some("B".into()));
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("A"), make_session("B")],
            test_ctx().synopsis,
        );

        assert_eq!(picker.cursor(), 2);
    }

    #[test]
    fn unknown_preselect_leaves_cursor_on_new_session_row() {
        let mut picker = SessionPickerView::with_preselect(Some("missing".into()));
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("A"), make_session("B")],
            test_ctx().synopsis,
        );

        assert_eq!(picker.cursor(), 0);
    }

    #[test]
    fn preselect_banner_includes_relative_updated_time() {
        let mut picker = SessionPickerView::with_preselect(Some("B".into()));
        let mut session = make_session("B");
        session.title = Some("Build fix".to_string());
        session.updated_at = Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        picker.set_sessions("test-brain".into(), vec![session], test_ctx().synopsis);

        let banner = picker.build_preselect_banner("B", test_ctx().synopsis, test_ctx().theme);
        let text = banner
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("Last: Build fix"));
        assert!(text.contains("5m ago"));
        assert!(text.contains("[Enter] resume"));
    }

    #[test]
    fn enter_on_non_current_session_does_not_wedge_picker_into_pending_state() {
        // Populated with 3 sessions, no "current session" so every row is
        // a resume candidate.
        let mut picker = SessionPickerView::new();
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("X"), make_session("Y"), make_session("Z")],
            test_ctx().synopsis,
        );
        // No current session id set — all rows are resume candidates.

        // Move cursor to row 1 (first session row, past [+ New]).
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );

        // First Enter: must return ResumeSession.
        let action1 = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &test_ctx(),
        );
        assert!(
            matches!(action1, Some(Action::ResumeSession { .. })),
            "expected ResumeSession, got {:?}",
            action1
        );

        // Second Down in the same frame: must NOT be silently eaten
        // by a pending-flag guard (the pre-fix behavior where `*resuming`
        // caused an unconditional early `None` return that swallowed all
        // subsequent input).
        //
        // We assert positively: the picker still processes cursor motion.
        let before_cursor = picker.debug_cursor();
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );
        let after_cursor = picker.debug_cursor();
        assert_ne!(
            before_cursor, after_cursor,
            "picker ignored input — resuming flag likely still present"
        );
    }
}

#[cfg(test)]
mod preview_render_tests {
    use super::*;
    use crate::session_metadata::SessionEntry;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, style::Color, Terminal};
    use spur_acp::{HistoryEntry, SessionId, SpurEventBody};
    use std::path::PathBuf;

    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        let mut rows = Vec::new();
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            rows.push(row);
        }
        rows
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut out = buffer_rows(buf).join("\n");
        out.push('\n');
        out
    }

    #[test]
    fn preview_prioritizes_state_then_intent_then_footer() {
        let mut synopsis = spur_core::SessionSynopsisProjection::new();
        synopsis.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("a1xxxxxx".into()),
            entries: vec![
                HistoryEntry {
                    role: "user".into(),
                    text: "original goal".into(),
                },
                HistoryEntry {
                    role: "assistant".into(),
                    text: "ack".into(),
                },
                HistoryEntry {
                    role: "user".into(),
                    text: "latest request".into(),
                },
            ],
        }));

        let lineage = spur_core::lineage::projection::ExecutorLineage::new();
        let plan_projection = spur_core::PlanProjectionStore::new();
        let brain_status = crate::app::BrainStatus::Idle;
        let ctx = crate::views::ViewContext {
            lineage: &lineage,
            plan_projection: &plan_projection,
            synopsis: &synopsis,
            brain_status: &brain_status,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let mut metadata = SessionMetadata::default();
        metadata.sessions.insert(
            "a1xxxxxx".into(),
            SessionEntry {
                draft: "unsent edit".into(),
                brain_name: Some("claude".into()),
                ..SessionEntry::default()
            },
        );

        let mut picker = SessionPickerView::new();
        picker.set_metadata(metadata);
        picker.set_sessions(
            "claude".into(),
            vec![SessionInfo::new(
                "a1xxxxxx".to_string(),
                PathBuf::from("/work/spur"),
            )],
            ctx.synopsis,
        );
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 24), &ctx))
            .unwrap();

        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Last: latest request"));
        assert!(text.contains("Draft: unsent edit"));
        assert!(text.contains("Intent: original goal"));
        assert!(text.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx"));
        assert!(!text.contains("Session: a1xxxxxx"));
        assert!(!text.contains("CWD: /work/spur"));
    }

    #[test]
    fn preview_renders_empty_state_placeholder_when_synopsis_missing() {
        let synopsis = spur_core::SessionSynopsisProjection::new();
        let lineage = spur_core::lineage::projection::ExecutorLineage::new();
        let plan_projection = spur_core::PlanProjectionStore::new();
        let brain_status = crate::app::BrainStatus::Idle;
        let ctx = crate::views::ViewContext {
            lineage: &lineage,
            plan_projection: &plan_projection,
            synopsis: &synopsis,
            brain_status: &brain_status,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let mut metadata = SessionMetadata::default();
        metadata.sessions.insert(
            "a1xxxxxx".into(),
            SessionEntry {
                brain_name: Some("claude".into()),
                ..SessionEntry::default()
            },
        );

        let mut picker = SessionPickerView::new();
        picker.set_metadata(metadata);
        picker.set_sessions(
            "claude".into(),
            vec![SessionInfo::new(
                "a1xxxxxx".to_string(),
                PathBuf::from("/work/spur"),
            )],
            ctx.synopsis,
        );
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 24), &ctx))
            .unwrap();

        let rows = buffer_rows(term.backend().buffer());
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("(resume to load message history)"));
        assert!(!text.contains("Intent:"));
        assert!(!text.contains("Last:"));
        assert!(text.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx"));
        let preview_border_row = rows
            .iter()
            .position(|row| row.contains(" Preview "))
            .expect("preview border should be visible");
        let placeholder_row = rows
            .iter()
            .enumerate()
            .skip(preview_border_row + 1)
            .find_map(|(y, row)| {
                row.contains("(resume to load message history)")
                    .then_some(y)
            })
            .expect("placeholder should be visible in preview");
        let placeholder_col = rows[placeholder_row]
            .find("(resume to load message history)")
            .expect("placeholder column should be visible");
        let placeholder_cell = term
            .backend()
            .buffer()
            .cell((placeholder_col as u16, placeholder_row as u16))
            .expect("placeholder cell should be in bounds");
        assert_eq!(
            placeholder_cell.style().fg,
            Some(Color::Rgb(0x60, 0x60, 0x60))
        );
        assert!(placeholder_cell
            .style()
            .add_modifier
            .contains(Modifier::ITALIC));
        let footer_row = rows
            .iter()
            .enumerate()
            .skip(preview_border_row + 1)
            .find_map(|(y, row)| {
                row.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx")
                    .then_some(y)
            })
            .expect("footer should be visible in preview");
        assert!(
            rows[preview_border_row + 1..=footer_row]
                .windows(2)
                .all(|pair| !(pair[0].trim().is_empty() && pair[1].trim().is_empty())),
            "preview should not render adjacent all-blank rows"
        );
    }

    #[test]
    fn preview_combines_draft_and_empty_state_when_synopsis_missing() {
        let synopsis = spur_core::SessionSynopsisProjection::new();
        let lineage = spur_core::lineage::projection::ExecutorLineage::new();
        let plan_projection = spur_core::PlanProjectionStore::new();
        let brain_status = crate::app::BrainStatus::Idle;
        let ctx = crate::views::ViewContext {
            lineage: &lineage,
            plan_projection: &plan_projection,
            synopsis: &synopsis,
            brain_status: &brain_status,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let mut metadata = SessionMetadata::default();
        metadata.sessions.insert(
            "a1xxxxxx".into(),
            SessionEntry {
                draft: "unsent edit".into(),
                brain_name: Some("claude".into()),
                ..SessionEntry::default()
            },
        );

        let mut picker = SessionPickerView::new();
        picker.set_metadata(metadata);
        picker.set_sessions(
            "claude".into(),
            vec![SessionInfo::new(
                "a1xxxxxx".to_string(),
                PathBuf::from("/work/spur"),
            )],
            ctx.synopsis,
        );
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 24), &ctx))
            .unwrap();

        let rows = buffer_rows(term.backend().buffer());
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Draft: unsent edit"));
        assert!(text.contains("(resume to load message history)"));

        let draft_row = rows
            .iter()
            .position(|row| row.contains("Draft: unsent edit"))
            .expect("draft should be visible in preview");
        let placeholder_row = rows
            .iter()
            .position(|row| row.contains("(resume to load message history)"))
            .expect("placeholder should be visible in preview");
        assert!(
            draft_row < placeholder_row,
            "draft should render above the empty-state placeholder"
        );
    }

    #[test]
    fn preview_renders_placeholder_for_slash_only_history() {
        let mut synopsis = spur_core::SessionSynopsisProjection::new();
        synopsis.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("a1xxxxxx".into()),
            entries: vec![
                HistoryEntry {
                    role: "user".into(),
                    text: "/clear".into(),
                },
                HistoryEntry {
                    role: "assistant".into(),
                    text: "cleared".into(),
                },
                HistoryEntry {
                    role: "user".into(),
                    text: "/help".into(),
                },
            ],
        }));

        let lineage = spur_core::lineage::projection::ExecutorLineage::new();
        let plan_projection = spur_core::PlanProjectionStore::new();
        let brain_status = crate::app::BrainStatus::Idle;
        let ctx = crate::views::ViewContext {
            lineage: &lineage,
            plan_projection: &plan_projection,
            synopsis: &synopsis,
            brain_status: &brain_status,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let mut metadata = SessionMetadata::default();
        metadata.sessions.insert(
            "a1xxxxxx".into(),
            SessionEntry {
                brain_name: Some("claude".into()),
                ..SessionEntry::default()
            },
        );

        let mut picker = SessionPickerView::new();
        picker.set_metadata(metadata);
        picker.set_sessions(
            "claude".into(),
            vec![SessionInfo::new(
                "a1xxxxxx".to_string(),
                PathBuf::from("/work/spur"),
            )],
            ctx.synopsis,
        );
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 24), &ctx))
            .unwrap();

        let rows = buffer_rows(term.backend().buffer());
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Last: /help"));
        assert!(text.contains("(resume to load message history)"));

        let preview_border_row = rows
            .iter()
            .position(|row| row.contains(" Preview "))
            .expect("preview border should be visible");
        let footer_row = rows
            .iter()
            .enumerate()
            .skip(preview_border_row + 1)
            .find_map(|(y, row)| {
                row.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx")
                    .then_some(y)
            })
            .expect("footer should be visible in preview");
        assert!(
            rows[preview_border_row + 1..=footer_row]
                .windows(2)
                .all(|pair| !(pair[0].trim().is_empty() && pair[1].trim().is_empty())),
            "preview should not render adjacent all-blank rows"
        );
    }

    #[test]
    fn preview_caps_long_intent_and_keeps_footer_visible() {
        let mut synopsis = spur_core::SessionSynopsisProjection::new();
        synopsis.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("a1xxxxxx".into()),
            entries: vec![
                HistoryEntry {
                    role: "user".into(),
                    text: "intent ".repeat(120),
                },
                HistoryEntry {
                    role: "assistant".into(),
                    text: "ack".into(),
                },
                HistoryEntry {
                    role: "user".into(),
                    text: "latest request".into(),
                },
            ],
        }));

        let lineage = spur_core::lineage::projection::ExecutorLineage::new();
        let plan_projection = spur_core::PlanProjectionStore::new();
        let brain_status = crate::app::BrainStatus::Idle;
        let ctx = crate::views::ViewContext {
            lineage: &lineage,
            plan_projection: &plan_projection,
            synopsis: &synopsis,
            brain_status: &brain_status,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let mut metadata = SessionMetadata::default();
        metadata
            .sessions
            .entry("a1xxxxxx".into())
            .or_default()
            .brain_name = Some("claude".into());

        let mut picker = SessionPickerView::new();
        picker.set_metadata(metadata);
        picker.set_sessions(
            "claude".into(),
            vec![SessionInfo::new(
                "a1xxxxxx".to_string(),
                PathBuf::from("/work/spur"),
            )],
            ctx.synopsis,
        );
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 24), &ctx))
            .unwrap();

        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Last: latest request"));
        assert!(text.contains("Intent: intent intent"));
        assert!(text.contains('…'));
        assert!(text.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx"));

        let rows = buffer_rows(term.backend().buffer());
        let preview_content_start = rows.len().saturating_sub(1 + PREVIEW_MAX_LINES as usize) + 1;
        let footer_row = rows
            .iter()
            .position(|row| row.contains("/work/spur \u{00b7} claude \u{00b7} a1xxxxxx"))
            .expect("footer should be visible in preview");
        let intent_value_rows = (preview_content_start..=footer_row)
            .filter(|&y| {
                rows[y].contains("intent")
                    && (0..term.backend().buffer().area.width).any(|x| {
                        term.backend().buffer()[(x, y as u16)].fg == Color::Rgb(128, 128, 128)
                    })
            })
            .count();

        assert_eq!(
            intent_value_rows, 3,
            "long intent should emit exactly three styled value lines"
        );
        assert!(
            footer_row >= preview_content_start,
            "footer must render inside the preview content area"
        );
        assert!(
            footer_row - preview_content_start < PREVIEW_MAX_LINES as usize,
            "preview content through footer must fit the 12-row budget"
        );
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_for_row;

    #[test]
    fn keeps_short_text_unchanged() {
        assert_eq!(truncate_for_row("short", 10), "short");
    }

    #[test]
    fn cuts_at_first_sentence_boundary() {
        assert_eq!(
            truncate_for_row("First sentence. Second one.", 100),
            "First sentence"
        );
    }

    #[test]
    fn cuts_at_first_question_mark() {
        assert_eq!(truncate_for_row("Why? Because.", 100), "Why");
    }

    #[test]
    fn cuts_at_newline() {
        assert_eq!(truncate_for_row("line one\nline two", 100), "line one");
    }

    #[test]
    fn cuts_at_grapheme_budget_with_ellipsis() {
        assert_eq!(truncate_for_row("abcdefghij", 5), "abcde\u{2026}");
    }

    #[test]
    fn handles_unicode_grapheme_clusters() {
        let s = "ééééé";
        assert_eq!(truncate_for_row(s, 3), "ééé\u{2026}");
    }

    #[test]
    fn returns_ellipsis_when_budget_under_one() {
        assert_eq!(truncate_for_row("anything", 0), "\u{2026}");
    }

    #[test]
    fn strips_leading_whitespace() {
        assert_eq!(truncate_for_row("   hello", 10), "hello");
    }
}

#[cfg(test)]
mod wrap_value_tests {
    use super::wrap_value;
    use unicode_width::UnicodeWidthStr;

    fn assert_widths(lines: &[String], width: usize) {
        for line in lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= width,
                "{line:?} exceeds width {width}"
            );
        }
    }

    #[test]
    fn returns_no_lines_when_max_lines_is_zero() {
        assert_eq!(wrap_value("anything", 5, 0), Vec::<String>::new());
    }

    #[test]
    fn handles_single_line_boundaries() {
        assert_eq!(wrap_value("abcde", 5, 1), vec!["abcde"]);
        assert_eq!(wrap_value("abcdef", 5, 1), vec!["abcd\u{2026}"]);
        assert_eq!(wrap_value("abcdef", 6, 1), vec!["abcdef"]);
    }

    #[test]
    fn caps_at_three_lines_and_marks_only_truncated_text() {
        assert_eq!(
            wrap_value("abcdefghijkl", 4, 3),
            vec!["abcd", "efgh", "ijkl"]
        );
        assert_eq!(
            wrap_value("abcdefghijklm", 4, 3),
            vec!["abcd", "efgh", "ijk\u{2026}"]
        );
    }

    #[test]
    fn respects_wide_chars_combining_marks_and_emoji() {
        assert_eq!(wrap_value("日本語", 4, 1), vec!["日\u{2026}"]);
        assert_eq!(
            wrap_value("e\u{301}e\u{301}e\u{301}", 2, 1),
            vec!["e\u{301}\u{2026}"]
        );
        assert_eq!(wrap_value("🙂🙂🙂", 4, 1), vec!["🙂\u{2026}"]);

        assert_widths(&wrap_value("日本語", 4, 2), 4);
        assert_widths(&wrap_value("🙂🙂🙂", 4, 2), 4);
        assert_eq!(wrap_value("日本語", 1, 3), vec!["日", "本", "語"]);
    }

    #[test]
    fn respects_embedded_newlines() {
        assert_eq!(wrap_value("ab\ncdef", 2, 3), vec!["ab", "cd", "ef"]);
        assert_eq!(wrap_value("ab\ncdefg", 2, 3), vec!["ab", "cd", "e\u{2026}"]);
        assert_eq!(wrap_value("ab\r\ncd", 2, 3), vec!["ab", "cd"]);
        assert_eq!(wrap_value("ab\rcd", 2, 3), vec!["ab", "cd"]);
    }

    #[test]
    fn trailing_newline_respects_line_budget() {
        assert_eq!(wrap_value("ab\n", 2, 1), vec!["ab"]);
        assert_eq!(wrap_value("ab\n", 2, 2), vec!["ab", ""]);
    }
}

#[cfg(test)]
mod resolve_label_tests {
    use super::*;
    use crate::session_metadata::SessionEntry;
    use spur_acp::SessionInfo;
    use spur_core::SessionSynopsis;
    use std::path::PathBuf;

    fn info_with_title(title: Option<&str>) -> SessionInfo {
        let mut info = SessionInfo::new("S1".to_string(), PathBuf::from("/tmp/proj"));
        info.title = title.map(|t| t.to_string());
        info
    }

    fn entry_with_override(t: Option<&str>) -> SessionEntry {
        SessionEntry {
            title_override: t.map(|s| s.to_string()),
            ..SessionEntry::default()
        }
    }

    fn synopsis_with_first(t: Option<&str>) -> SessionSynopsis {
        SessionSynopsis {
            first_user_msg: t.map(|s| s.to_string()),
            last_user_msg: None,
        }
    }

    #[test]
    fn title_override_wins_over_everything() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(Some("manual rename"));
        let synopsis = synopsis_with_first(Some("first user msg"));
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "manual rename"
        );
    }

    #[test]
    fn first_user_msg_beats_agent_title_when_no_override() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(None);
        let synopsis = synopsis_with_first(Some("real intent"));
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "real intent"
        );
    }

    #[test]
    fn agent_title_used_when_no_synopsis() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(None);
        let synopsis = synopsis_with_first(None);
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "agent title"
        );
    }

    #[test]
    fn cwd_fallback_when_no_title_or_synopsis() {
        let info = info_with_title(None);
        let entry = entry_with_override(None);
        assert_eq!(resolve_label(&info, Some(&entry), None, true, 60), "proj/");
    }

    #[test]
    fn final_fallback_to_untitled_session() {
        let info = info_with_title(None);
        assert_eq!(
            resolve_label(&info, None, None, false, 60),
            "(untitled session)"
        );
    }

    #[test]
    fn empty_string_override_is_skipped() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(Some(""));
        let synopsis = synopsis_with_first(Some("first user msg"));
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "first user msg"
        );
    }
}

#[cfg(test)]
mod filter_haystack_tests {
    use super::*;
    use crate::session_metadata::SessionMetadata;
    use spur_core::{SessionSynopsis, SessionSynopsisProjection};
    use std::path::PathBuf;

    fn make_session(id: &str, title: Option<&str>) -> SessionInfo {
        let mut s = SessionInfo::new(id.to_string(), PathBuf::from("/tmp"));
        s.title = title.map(|t| t.to_string());
        s
    }

    fn set_filter(picker: &mut SessionPickerView, value: &str) {
        let PickerState::Populated { filter, .. } = &mut picker.state else {
            panic!("picker should be populated");
        };
        *filter = value.to_string();
    }

    #[test]
    fn filter_matches_first_user_msg_even_when_label_does_not() {
        let sessions = vec![make_session("S1", Some("Build fix"))];
        let metadata = SessionMetadata::default();

        let mut synopsis = SessionSynopsisProjection::new();
        synopsis.insert_for_test(
            spur_acp::SessionId("S1".into()),
            SessionSynopsis {
                first_user_msg: Some("refactor auth callers".into()),
                last_user_msg: Some("ack".into()),
            },
        );

        let indices = filtered_indices(&sessions, "auth", &metadata, &synopsis, false);
        assert_eq!(
            indices,
            vec![0],
            "filter 'auth' should match synopsis content"
        );
    }

    #[test]
    fn haystack_picks_up_late_synopsis_updates_without_refresh() {
        let mut picker = SessionPickerView::new();
        let sessions = vec![make_session("S1", Some("Build fix"))];
        let mut synopsis = SessionSynopsisProjection::new();
        synopsis.insert_for_test(
            spur_acp::SessionId("S1".into()),
            SessionSynopsis {
                first_user_msg: Some("alpha tag".into()),
                last_user_msg: None,
            },
        );

        picker.set_sessions("agent".into(), sessions, &synopsis);

        synopsis.insert_for_test(
            spur_acp::SessionId("S1".into()),
            SessionSynopsis {
                first_user_msg: Some("beta tag".into()),
                last_user_msg: None,
            },
        );

        set_filter(&mut picker, "alpha");
        assert_eq!(
            picker.visible_session_count(&synopsis),
            0,
            "old synopsis content should not remain cached"
        );

        set_filter(&mut picker, "beta");
        assert_eq!(
            picker.visible_session_count(&synopsis),
            1,
            "should find session by late 'beta' synopsis update"
        );
    }
}
