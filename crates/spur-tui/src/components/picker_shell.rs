//! Popup shell that owns a query surface (MiniInput when the source is
//! OwnedByShell) and drives a CompletionPopup for row selection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::components::mini_input::MiniInput;
use crate::components::picker_popover::PickerPopover;
use crate::components::query_source::{
    QueryMode, QuerySource, RetrievalAccept, RetrievalPreview, RetrievalRow,
};
use crate::theme::{resolve_token, ColorDepth, Theme};

pub(crate) fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverAnchor {
    Right,
    Left,
    Above,
    Suppressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickerLayout {
    pub picker_rect: Rect,
    pub popover_rect: Option<Rect>,
    pub popover_anchor: PopoverAnchor,
}

pub fn compute_picker_layout(
    container: Rect,
    anchor: Rect,
    picker_size: (u16, u16),
    popover_size: Option<(u16, u16)>,
) -> PickerLayout {
    let (picker_width, picker_height) = picker_size;
    let picker_x = anchor
        .x
        .saturating_add(2)
        .min(container.x + container.width.saturating_sub(picker_width));
    let picker_y = anchor.y.saturating_sub(picker_height);
    let picker_rect = Rect::new(picker_x, picker_y, picker_width, picker_height);

    let Some((popover_width, popover_height)) = popover_size else {
        return PickerLayout {
            picker_rect,
            popover_rect: None,
            popover_anchor: PopoverAnchor::Suppressed,
        };
    };

    let right_x = picker_rect
        .x
        .saturating_add(picker_rect.width)
        .saturating_add(1);
    let right_fits = right_x.saturating_add(popover_width) <= container.x + container.width;
    if right_fits {
        let y = picker_rect
            .y
            .min(container.y + container.height.saturating_sub(popover_height));
        return PickerLayout {
            picker_rect,
            popover_rect: Some(Rect::new(right_x, y, popover_width, popover_height)),
            popover_anchor: PopoverAnchor::Right,
        };
    }

    let left_x = picker_rect
        .x
        .saturating_sub(popover_width.saturating_add(1));
    let left_fits = picker_rect.x >= container.x.saturating_add(popover_width.saturating_add(1));
    if left_fits {
        let y = picker_rect
            .y
            .min(container.y + container.height.saturating_sub(popover_height));
        return PickerLayout {
            picker_rect,
            popover_rect: Some(Rect::new(left_x, y, popover_width, popover_height)),
            popover_anchor: PopoverAnchor::Left,
        };
    }

    let above_y = picker_rect
        .y
        .saturating_sub(popover_height.saturating_add(1));
    let above_fits = picker_rect.y >= container.y.saturating_add(popover_height.saturating_add(1));
    if above_fits {
        return PickerLayout {
            picker_rect,
            popover_rect: Some(Rect::new(
                picker_rect.x,
                above_y,
                popover_width,
                popover_height,
            )),
            popover_anchor: PopoverAnchor::Above,
        };
    }

    PickerLayout {
        picker_rect,
        popover_rect: None,
        popover_anchor: PopoverAnchor::Suppressed,
    }
}

/// Result of handling a key event inside the shell.
#[derive(Debug)]
pub enum PickerAction {
    /// Key was consumed; shell stays open with possibly new state.
    None,
    /// User accepted a row; dispatch this and close the shell.
    Accept(RetrievalAccept),
    /// User cancelled (Esc); close the shell without mutation.
    Cancel,
}

/// Popup shell wrapping a query surface + row list.
pub struct PickerShell {
    source: Box<dyn QuerySource>,
    query: MiniInput,
    rows: Vec<RetrievalRow>,
    list_state: ListState,
    active_preview: Option<(usize, RetrievalPreview)>,
}

impl PickerShell {
    /// Open a shell over the given source. Immediately refreshes with an
    /// empty query to populate initial rows.
    pub fn open(mut source: Box<dyn QuerySource>) -> Self {
        let rows = source.refresh("");
        let mut list_state = ListState::default();
        list_state.select(first_selectable_index(&rows));
        Self {
            source,
            query: MiniInput::new(),
            rows,
            list_state,
            active_preview: None,
        }
    }

    /// Open a shell with an initial query (e.g. from an active trigger
    /// prefix). For `ReadFromInputBar` sources, installs `query` into the
    /// shell's internal `MiniInput` via `set_query_from_input_bar`. For
    /// `OwnedByShell` sources, uses `query` as the initial MiniInput text.
    pub fn open_with_query(source: Box<dyn QuerySource>, query: &str) -> Self {
        let mut shell = Self::open(source);
        if !query.is_empty() {
            // Use the existing set_query_from_input_bar path for trigger
            // sources; for OwnedByShell sources, fall back to pasting into
            // the MiniInput directly.
            if shell.source.query_mode() == QueryMode::ReadFromInputBar {
                shell.set_query_from_input_bar(query);
            } else {
                shell.query.paste(query);
                shell.rows = shell.source.refresh(shell.query.text());
                shell.active_preview = None;
                shell.list_state.select(first_selectable_index(&shell.rows));
            }
        }
        shell
    }

    // ── Test accessors ─────────────────────────────────────────────────
    #[cfg(test)]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    #[cfg(test)]
    pub fn row_primaries(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.primary.clone()).collect()
    }

    #[cfg(test)]
    pub fn title(&self) -> &str {
        self.source.title()
    }

    #[cfg(test)]
    pub fn set_selected_index_for_test(&mut self, idx: Option<usize>) {
        self.list_state.select(idx);
    }

    // ── Key handling ───────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Up => {
                self.select_prev();
                PickerAction::None
            }
            KeyCode::Down => {
                self.select_next();
                PickerAction::None
            }
            KeyCode::PageUp if self.source.query_mode() == QueryMode::ReadFromInputBar => {
                self.select_prev_page();
                PickerAction::None
            }
            KeyCode::PageDown if self.source.query_mode() == QueryMode::ReadFromInputBar => {
                self.select_next_page();
                PickerAction::None
            }
            KeyCode::Home if self.source.query_mode() == QueryMode::ReadFromInputBar => {
                self.select_first();
                PickerAction::None
            }
            KeyCode::End if self.source.query_mode() == QueryMode::ReadFromInputBar => {
                self.select_last();
                PickerAction::None
            }
            KeyCode::Tab | KeyCode::Enter => self.accept_selected(),
            KeyCode::Backspace if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.backspace();
                self.refilter();
                PickerAction::None
            }
            KeyCode::Delete if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.delete();
                self.refilter();
                PickerAction::None
            }
            KeyCode::Left if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.left();
                PickerAction::None
            }
            KeyCode::Right if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.right();
                PickerAction::None
            }
            KeyCode::Home if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.home();
                PickerAction::None
            }
            KeyCode::End if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.end();
                PickerAction::None
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.source.query_mode() == QueryMode::OwnedByShell =>
            {
                self.query.insert_char(c);
                self.refilter();
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() || !self.rows.iter().any(|r| r.selectable) {
            self.active_preview = None;
            self.list_state.select(None);
            return;
        }
        let i = prev_selectable_index(&self.rows, self.list_state.selected());
        if self.list_state.selected() != Some(i) {
            self.active_preview = None;
        }
        self.list_state.select(Some(i));
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() || !self.rows.iter().any(|r| r.selectable) {
            self.active_preview = None;
            self.list_state.select(None);
            return;
        }
        let i = next_selectable_index(&self.rows, self.list_state.selected());
        if self.list_state.selected() != Some(i) {
            self.active_preview = None;
        }
        self.list_state.select(Some(i));
    }

    fn select_prev_page(&mut self) {
        for _ in 0..5 {
            self.select_prev();
        }
    }

    fn select_next_page(&mut self) {
        for _ in 0..5 {
            self.select_next();
        }
    }

    fn select_first(&mut self) {
        self.list_state.select(first_selectable_index(&self.rows));
        self.active_preview = None;
    }

    fn select_last(&mut self) {
        self.list_state.select(last_selectable_index(&self.rows));
        self.active_preview = None;
    }

    fn accept_selected(&self) -> PickerAction {
        let Some(idx) = self.list_state.selected() else {
            return PickerAction::Cancel;
        };
        if self.rows.get(idx).is_some_and(|row| !row.selectable) {
            return PickerAction::None;
        }
        match self.source.accept(idx) {
            Some(a) => PickerAction::Accept(a),
            None => PickerAction::Cancel,
        }
    }

    /// Refresh rows from the source using the current query.
    /// Resets the selection to the top (0) so the best fuzzy match is highlighted.
    fn refilter(&mut self) {
        self.active_preview = None;
        self.rows = self.source.refresh(self.query.text());
        self.list_state.select(first_selectable_index(&self.rows));
    }

    /// For mention/slash (`QueryMode::ReadFromInputBar`). Called by the
    /// view on every InputBar text change so the shell's query mirrors
    /// the trigger prefix.
    pub fn set_query_from_input_bar(&mut self, q: &str) {
        debug_assert_eq!(self.source.query_mode(), QueryMode::ReadFromInputBar);
        // Directly install the prefix text; no edit history to preserve.
        self.query.clear();
        self.query.paste(q);
        self.refilter();
    }

    /// The underlying source's query mode. Used by the view's key-routing
    /// branch to distinguish trigger-driven (`ReadFromInputBar`) shells
    /// from history (`OwnedByShell`) shells without maintaining a parallel
    /// trigger-state field.
    pub fn query_mode(&self) -> crate::components::query_source::QueryMode {
        self.source.query_mode()
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render above `anchor` (the InputBar's rect), clipped to `container`.
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect, theme: &Theme) {
        let query_mode_owned = self.source.query_mode() == QueryMode::OwnedByShell;
        self.update_active_preview();
        let has_preview = self.active_preview.is_some();
        let list_rows = self.rows.len().clamp(1, 8) as u16;
        let query_rows = if query_mode_owned { 1 } else { 0 };
        let inner_rows = list_rows + query_rows;
        let popup_height = inner_rows + 2; // +2 for block border

        let popup_width = if has_preview {
            (container.width / 2).clamp(40, 60)
        } else {
            (container.width / 2).clamp(30, container.width)
        };
        let popover_size = if has_preview {
            let width = 80u16.min(
                container
                    .width
                    .saturating_sub(popup_width.saturating_add(3)),
            );
            let height = 18u16.min(container.height.saturating_sub(2));
            (width > 0 && height > 0).then_some((width, height))
        } else {
            None
        };
        let layout =
            compute_picker_layout(container, anchor, (popup_width, popup_height), popover_size);
        let popup_area = layout.picker_rect;

        frame.render_widget(Clear, popup_area);

        let title = format!(" {} ", self.source.title());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "picker.match.fg")))
            .title(Span::styled(
                title,
                Style::default().fg(token(theme, "picker.match.fg")),
            ));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut cursor_cell: Option<(u16, u16)> = None;
        let list_column = inner;

        let list_area = if query_mode_owned {
            // Render query line at top of inner area.
            let q_area = Rect::new(list_column.x, list_column.y, list_column.width, 1);
            let prompt = "search: ";
            let prompt_len = prompt.len() as u16;
            let q_text = self.query.text();
            let line = Line::from(vec![
                Span::styled(prompt, Style::default().fg(token(theme, "picker.hint.fg"))),
                Span::raw(q_text.to_string()),
            ]);
            frame.render_widget(Paragraph::new(line), q_area);
            // Cursor placement: prompt + cursor byte offset (fine for
            // monospace ASCII; multi-byte alignment is a Phase 2 polish).
            let cx = q_area.x + prompt_len + self.query.cursor() as u16;
            let cy = q_area.y;
            cursor_cell = Some((cx, cy));
            Rect::new(
                list_column.x,
                list_column.y + 1,
                list_column.width,
                list_column.height.saturating_sub(1),
            )
        } else {
            list_column
        };

        self.render_rows(frame, list_area, theme);

        if let Some((cx, cy)) = cursor_cell {
            if cx < popup_area.x + popup_area.width && cy < popup_area.y + popup_area.height {
                frame.set_cursor_position((cx, cy));
            }
        }

        if has_preview && !matches!(layout.popover_anchor, PopoverAnchor::Suppressed) {
            if let (Some((_, preview)), Some(popover_rect)) =
                (self.active_preview.as_ref(), layout.popover_rect)
            {
                frame.render_widget(Clear, popover_rect);
                PickerPopover { preview, theme }.render(frame, popover_rect);
            }
        }
    }

    fn update_active_preview(&mut self) {
        let Some(idx) = self.list_state.selected() else {
            self.active_preview = None;
            return;
        };
        if matches!(self.active_preview.as_ref(), Some((cached_idx, _)) if *cached_idx == idx) {
            return;
        }
        self.active_preview = self.source.preview_for(idx).map(|preview| (idx, preview));
    }

    fn render_rows(&self, frame: &mut Frame, list_area: Rect, theme: &Theme) {
        if self.rows.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                "No matches. Type to refine, Esc to dismiss.",
                Style::default().fg(token(theme, "picker.hint.fg")),
            )));
            frame.render_widget(p, list_area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let mut spans = Vec::with_capacity(4);
                // Primary with atom-span styling.
                if r.atoms.is_empty() {
                    let base_style = if r.dimmed {
                        Style::default().fg(token(theme, "picker.hint.fg"))
                    } else {
                        Style::default()
                            .fg(token(theme, "picker.row.fg"))
                            .add_modifier(Modifier::BOLD)
                    };
                    spans.push(Span::styled(r.primary.clone(), base_style));
                } else {
                    let mut cursor = 0usize;
                    for &(a, b) in &r.atoms {
                        if a > cursor && a <= r.primary.len() {
                            spans.push(Span::styled(
                                r.primary[cursor..a].to_string(),
                                Style::default()
                                    .fg(token(theme, "picker.row.fg"))
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        let end = b.min(r.primary.len());
                        if end > a {
                            spans.push(Span::styled(
                                r.primary[a..end].to_string(),
                                Style::default()
                                    .fg(token(theme, "picker.match.fg"))
                                    .add_modifier(Modifier::UNDERLINED),
                            ));
                            cursor = end;
                        }
                    }
                    if cursor < r.primary.len() {
                        spans.push(Span::styled(
                            r.primary[cursor..].to_string(),
                            Style::default()
                                .fg(token(theme, "picker.row.fg"))
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
                if !r.secondary.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.secondary.clone(),
                        if r.dimmed {
                            Style::default().fg(token(theme, "picker.hint.fg"))
                        } else {
                            Style::default().fg(token(theme, "picker.row.fg"))
                        },
                    ));
                }
                if !r.tag.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.tag.clone(),
                        Style::default().fg(token(theme, "picker.hint.fg")),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::default().bg(token(theme, "picker.selected.bg")));
        let mut list_state = self.list_state.clone();
        frame.render_stateful_widget(list, list_area, &mut list_state);
    }
}

fn first_selectable_index(rows: &[RetrievalRow]) -> Option<usize> {
    rows.iter().position(|row| row.selectable)
}

fn last_selectable_index(rows: &[RetrievalRow]) -> Option<usize> {
    rows.iter().rposition(|row| row.selectable)
}

fn next_selectable_index(rows: &[RetrievalRow], selected: Option<usize>) -> usize {
    let len = rows.len();
    let start = selected.unwrap_or(0);
    for step in 1..=len {
        let idx = (start + step) % len;
        if rows[idx].selectable {
            return idx;
        }
    }
    start
}

fn prev_selectable_index(rows: &[RetrievalRow], selected: Option<usize>) -> usize {
    let len = rows.len();
    let start = selected.unwrap_or(0);
    for step in 1..=len {
        let idx = (start + len - (step % len)) % len;
        if rows[idx].selectable {
            return idx;
        }
    }
    start
}

fn truncate_preview_lines(
    lines: Vec<Line<'static>>,
    max_rows: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if lines.len() <= max_rows {
        return lines;
    }
    if max_rows == 0 {
        return Vec::new();
    }

    let dropped = lines.len().saturating_sub(max_rows.saturating_sub(1));
    let keep = max_rows.saturating_sub(1);
    let mut out: Vec<Line<'static>> = lines.into_iter().take(keep).collect();
    out.push(Line::from(Span::styled(
        format!("  +{dropped} more …"),
        Style::default()
            .fg(token(theme, "picker.hint.fg"))
            .add_modifier(Modifier::ITALIC),
    )));
    out
}

pub(crate) fn truncate_preview_lines_to_fit(
    lines: Vec<Line<'static>>,
    max_rows: usize,
    max_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if max_rows == 0 {
        return Vec::new();
    }
    if preview_visual_rows(&lines, max_width) <= max_rows {
        return lines;
    }

    let mut used_rows = 0usize;
    let mut keep_count = 0usize;
    let row_budget = max_rows.saturating_sub(1);
    for line in &lines {
        let line_rows = wrapped_line_rows(line, max_width);
        if used_rows.saturating_add(line_rows) > row_budget {
            break;
        }
        used_rows += line_rows;
        keep_count += 1;
    }

    truncate_preview_lines(lines, keep_count.saturating_add(1), theme)
}

fn preview_visual_rows(lines: &[Line<'static>], max_width: usize) -> usize {
    lines
        .iter()
        .map(|line| wrapped_line_rows(line, max_width))
        .sum()
}

fn wrapped_line_rows(line: &Line<'static>, max_width: usize) -> usize {
    if max_width == 0 {
        return 1;
    }
    line.width().max(1).div_ceil(max_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::query_source::{HistoryQuerySource, RetrievalAccept, RetrievalPreview};
    use crate::input_history::{InputHistoryEntry, InputStateSnapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
    use std::cell::Cell;
    use std::rc::Rc;

    fn mk(text: &str) -> InputHistoryEntry {
        InputHistoryEntry::new(InputStateSnapshot::from_text(text))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn rendered_shell_buffer(shell: &mut PickerShell, width: u16, height: u16) -> Buffer {
        rendered_shell_buffer_with_anchor_x(shell, width, height, 0)
    }

    fn rendered_shell_buffer_with_anchor_x(
        shell: &mut PickerShell,
        width: u16,
        height: u16,
        anchor_x: u16,
    ) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let anchor = Rect::new(anchor_x, height - 1, width.saturating_sub(anchor_x), 1);
                let container = Rect::new(0, 0, width, height);
                shell.render(f, anchor, container, crate::theme::fallback_theme());
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn rendered_shell_text(shell: &mut PickerShell, width: u16) -> String {
        let height = 12;
        let buffer = rendered_shell_buffer(shell, width, height);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).expect("in-bounds buffer cell").symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn find_text_start(
        buffer: &Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        let needle_chars: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
        if needle_chars.is_empty() {
            return None;
        }
        for y in 0..height {
            for x in 0..width.saturating_sub(needle_chars.len() as u16 - 1) {
                let matches = needle_chars.iter().enumerate().all(|(offset, expected)| {
                    buffer
                        .cell((x + offset as u16, y))
                        .is_some_and(|cell| cell.symbol() == expected)
                });
                if matches {
                    return Some((x, y));
                }
            }
        }
        None
    }

    struct PreviewSource;

    impl QuerySource for PreviewSource {
        fn title(&self) -> &str {
            "Mentions · @"
        }

        fn query_mode(&self) -> QueryMode {
            QueryMode::ReadFromInputBar
        }

        fn refresh(&mut self, _query: &str) -> Vec<RetrievalRow> {
            vec![RetrievalRow {
                primary: "🎟 bd-1234 Short issue row".to_string(),
                secondary: "open · alice".to_string(),
                tag: "P2".to_string(),
                atoms: Vec::new(),
                selectable: true,
                dimmed: false,
            }]
        }

        fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
            None
        }

        fn preview_for(&self, row_idx: usize) -> Option<RetrievalPreview> {
            (row_idx == 0).then(|| RetrievalPreview::Text {
                title: "bd-1234".to_string(),
                lines: vec![Line::raw("Preview-only disambiguating title")],
            })
        }
    }

    struct CountingPreviewSource {
        count: Rc<Cell<usize>>,
    }

    impl QuerySource for CountingPreviewSource {
        fn title(&self) -> &str {
            "Mentions · @"
        }

        fn query_mode(&self) -> QueryMode {
            QueryMode::ReadFromInputBar
        }

        fn refresh(&mut self, _query: &str) -> Vec<RetrievalRow> {
            vec![
                RetrievalRow {
                    primary: "bd-1 First".to_string(),
                    secondary: String::new(),
                    tag: String::new(),
                    atoms: Vec::new(),
                    selectable: true,
                    dimmed: false,
                },
                RetrievalRow {
                    primary: "bd-2 Second".to_string(),
                    secondary: String::new(),
                    tag: String::new(),
                    atoms: Vec::new(),
                    selectable: true,
                    dimmed: false,
                },
            ]
        }

        fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
            None
        }

        fn preview_for(&self, row_idx: usize) -> Option<RetrievalPreview> {
            self.count.set(self.count.get() + 1);
            Some(RetrievalPreview::Text {
                title: format!("bd-{}", row_idx + 1),
                lines: vec![Line::raw(format!("preview {}", row_idx + 1))],
            })
        }
    }

    struct MentionSectionSource;

    impl QuerySource for MentionSectionSource {
        fn title(&self) -> &str {
            "Mentions · @"
        }

        fn query_mode(&self) -> QueryMode {
            QueryMode::ReadFromInputBar
        }

        fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
            if !query.is_empty() {
                return vec![
                    content_row("🤖 @worker:alpha"),
                    content_row("📄 @src/lib.rs"),
                ];
            }
            vec![
                header_row("── Workers ──"),
                content_row("🤖 @worker:alpha"),
                header_row("── Files ──"),
                content_row("📄 @src/lib.rs"),
                header_row("── Issues ──"),
                content_row("🎟 bd-12 Build picker"),
                header_row("── Code ──"),
                content_row("🗎 @src/main.rs"),
            ]
        }

        fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
            None
        }
    }

    fn header_row(primary: &str) -> RetrievalRow {
        RetrievalRow {
            primary: primary.to_string(),
            secondary: String::new(),
            tag: String::new(),
            atoms: Vec::new(),
            selectable: false,
            dimmed: true,
        }
    }

    fn content_row(primary: &str) -> RetrievalRow {
        RetrievalRow {
            primary: primary.to_string(),
            secondary: String::new(),
            tag: String::new(),
            atoms: Vec::new(),
            selectable: true,
            dimmed: false,
        }
    }

    #[test]
    fn open_populates_with_empty_query_rows() {
        let src = HistoryQuerySource::new(vec![mk("alpha"), mk("beta")]);
        let shell = PickerShell::open(Box::new(src));
        assert_eq!(shell.row_count(), 2);
        assert_eq!(shell.query(), "");
        assert_eq!(shell.selected_index(), Some(0));
    }

    #[test]
    fn typing_filters_rows() {
        let src = HistoryQuerySource::new(vec![mk("alpha"), mk("beta")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Char('b')));
        assert_eq!(shell.query(), "b");
        assert_eq!(shell.row_count(), 1);
    }

    #[test]
    fn arrow_keys_navigate_rows() {
        let src = HistoryQuerySource::new(vec![mk("a"), mk("b"), mk("c")]);
        let mut shell = PickerShell::open(Box::new(src));
        assert_eq!(shell.selected_index(), Some(0));
        shell.handle_key(key(KeyCode::Down));
        assert_eq!(shell.selected_index(), Some(1));
        shell.handle_key(key(KeyCode::Up));
        assert_eq!(shell.selected_index(), Some(0));
    }

    #[test]
    fn selection_resets_to_top_on_refilter() {
        let src = HistoryQuerySource::new(vec![mk("apple pie"), mk("apple juice"), mk("banana")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Down)); // select row 1
        shell.handle_key(key(KeyCode::Char('a'))); // filter — all three still match "a"
                                                   // The cursor should always reset to the top (0) when the query changes
                                                   // so the best match is selected.
        assert_eq!(shell.selected_index(), Some(0));
    }

    #[test]
    fn tab_returns_accept_action() {
        let src = HistoryQuerySource::new(vec![mk("hello")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Tab));
        match act {
            PickerAction::Accept(RetrievalAccept::ReplaceState(snap)) => {
                assert_eq!(snap.text, "hello");
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn enter_returns_accept_action_for_history() {
        let src = HistoryQuerySource::new(vec![mk("hello")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            act,
            PickerAction::Accept(RetrievalAccept::ReplaceState(_))
        ));
    }

    #[test]
    fn esc_returns_cancel_action() {
        let src = HistoryQuerySource::new(vec![mk("x")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Esc));
        assert!(matches!(act, PickerAction::Cancel));
    }

    #[test]
    fn backspace_shortens_query() {
        let src = HistoryQuerySource::new(vec![mk("ab")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Char('a')));
        shell.handle_key(key(KeyCode::Char('b')));
        shell.handle_key(key(KeyCode::Backspace));
        assert_eq!(shell.query(), "a");
    }

    #[test]
    fn query_mode_accessor_matches_underlying_source() {
        use crate::components::query_source::QueryMode;
        let hist_src = HistoryQuerySource::new(vec![mk("a")]);
        let shell = PickerShell::open(Box::new(hist_src));
        assert_eq!(shell.query_mode(), QueryMode::OwnedByShell);
    }

    #[test]
    fn accept_on_empty_rows_returns_cancel() {
        let src = HistoryQuerySource::new(Vec::new());
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Enter));
        assert!(matches!(act, PickerAction::Cancel));
    }

    #[test]
    fn wide_issue_picker_renders_preview_pane() {
        let mut shell = PickerShell::open(Box::new(PreviewSource));
        let width = 120;
        let height = 12;
        let anchor_x = 0;

        let buffer = rendered_shell_buffer_with_anchor_x(&mut shell, width, height, anchor_x);
        let preview_pos =
            find_text_start(&buffer, width, height, "Preview-only disambiguating title")
                .expect("preview title coordinate");
        let anchor = Rect::new(anchor_x, height - 1, width.saturating_sub(anchor_x), 1);
        let container = Rect::new(0, 0, width, height);
        let layout = compute_picker_layout(container, anchor, (60, 3), Some((57, 10)));
        let picker_right_edge = layout
            .picker_rect
            .x
            .saturating_add(layout.picker_rect.width)
            .saturating_sub(1);

        assert!(
            preview_pos.0 > picker_right_edge,
            "preview should render to the right of picker: preview={preview_pos:?} picker={:?}",
            layout.picker_rect
        );
    }

    #[test]
    fn narrow_issue_picker_renders_preview_with_fallback() {
        let mut shell = PickerShell::open(Box::new(PreviewSource));

        let text = rendered_shell_text(&mut shell, 80);

        assert!(text.contains("Preview-only disambiguating title"), "{text}");
    }

    #[test]
    fn very_narrow_issue_picker_suppresses_popover() {
        let mut shell = PickerShell::open(Box::new(PreviewSource));

        let text = rendered_shell_text(&mut shell, 50);

        assert!(
            !text.contains("Preview-only disambiguating title"),
            "{text}"
        );
    }

    #[test]
    fn preview_body_shows_truncation_trailer_when_area_is_short() {
        let lines = (1..=20)
            .map(|n| Line::raw(format!("line-{n:02}")))
            .collect();
        let rendered = truncate_preview_lines_to_fit(
            lines,
            4,  // 3 lines + trailer
            40, // wide enough to avoid wrapping
            crate::theme::fallback_theme(),
        );
        let text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("+17 more"), "{text}");
        for n in 4..=20 {
            assert!(!text.contains(&format!("line-{n:02}")), "{text}");
        }
    }

    #[test]
    fn preview_for_is_cached_until_selection_changes() {
        let count = Rc::new(Cell::new(0));
        let mut shell = PickerShell::open(Box::new(CountingPreviewSource {
            count: Rc::clone(&count),
        }));

        let _ = rendered_shell_text(&mut shell, 120);
        let _ = rendered_shell_text(&mut shell, 120);
        assert_eq!(count.get(), 1);

        shell.handle_key(key(KeyCode::Down));
        let _ = rendered_shell_text(&mut shell, 120);
        let _ = rendered_shell_text(&mut shell, 120);
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn compute_picker_layout_prefers_right_sidecar() {
        let container = Rect::new(0, 0, 120, 40);
        let anchor = Rect::new(20, 30, 50, 1);
        let layout = compute_picker_layout(container, anchor, (60, 10), Some((30, 10)));
        assert_eq!(layout.popover_anchor, PopoverAnchor::Right);
        assert_eq!(layout.popover_rect, Some(Rect::new(83, 20, 30, 10)));
    }

    #[test]
    fn compute_picker_layout_mirrors_left_at_right_edge() {
        let container = Rect::new(0, 0, 120, 40);
        let anchor = Rect::new(90, 30, 30, 1);
        let layout = compute_picker_layout(container, anchor, (60, 10), Some((30, 10)));
        assert_eq!(layout.popover_anchor, PopoverAnchor::Left);
        assert_eq!(layout.popover_rect, Some(Rect::new(29, 20, 30, 10)));
    }

    #[test]
    fn compute_picker_layout_suppresses_when_vertical_too_tight() {
        let container = Rect::new(0, 0, 40, 8);
        let anchor = Rect::new(1, 7, 10, 1);
        let layout = compute_picker_layout(container, anchor, (30, 6), Some((25, 4)));
        assert_eq!(layout.popover_anchor, PopoverAnchor::Suppressed);
        assert_eq!(layout.popover_rect, None);
    }

    #[test]
    fn compute_picker_layout_stacks_above_on_tall_narrow_terminal() {
        let container = Rect::new(0, 0, 42, 40);
        let anchor = Rect::new(1, 30, 10, 1);
        let layout = compute_picker_layout(container, anchor, (30, 8), Some((35, 6)));
        assert_eq!(layout.popover_anchor, PopoverAnchor::Above);
        assert_eq!(layout.popover_rect, Some(Rect::new(3, 15, 35, 6)));
    }

    #[test]
    fn mention_headers_render_in_order_and_navigation_skips_them() {
        let mut shell = PickerShell::open(Box::new(MentionSectionSource));
        assert_eq!(
            shell.row_primaries(),
            vec![
                "── Workers ──",
                "🤖 @worker:alpha",
                "── Files ──",
                "📄 @src/lib.rs",
                "── Issues ──",
                "🎟 bd-12 Build picker",
                "── Code ──",
                "🗎 @src/main.rs",
            ]
        );
        assert_eq!(shell.selected_index(), Some(1));

        shell.handle_key(key(KeyCode::Down));
        assert_eq!(shell.selected_index(), Some(3));
        shell.handle_key(key(KeyCode::Down));
        assert_eq!(shell.selected_index(), Some(5));
        shell.handle_key(key(KeyCode::Up));
        assert_eq!(shell.selected_index(), Some(3));
    }

    #[test]
    fn mention_header_accept_is_noop_and_typed_query_has_no_headers() {
        let mut shell = PickerShell::open(Box::new(MentionSectionSource));
        shell.set_selected_index_for_test(Some(0));
        assert!(matches!(
            shell.handle_key(key(KeyCode::Enter)),
            PickerAction::None
        ));

        shell.set_query_from_input_bar("wo");
        assert!(shell.row_primaries().iter().all(|row| !row.contains("──")));
    }
}
