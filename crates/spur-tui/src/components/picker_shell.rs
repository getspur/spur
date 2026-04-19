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
use crate::components::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

/// Result of handling a key event inside the shell.
#[derive(Debug)]
pub enum PickerAction {
    /// Key was consumed; shell stays open with possibly new state.
    None,
    /// User accepted a row; dispatch this and close the shell.
    Accept(RetrievalAccept),
    /// User cancelled (Esc/Ctrl+C); close the shell without mutation.
    Cancel,
}

/// Popup shell wrapping a query surface + row list.
pub struct PickerShell {
    source: Box<dyn QuerySource>,
    query: MiniInput,
    rows: Vec<RetrievalRow>,
    list_state: ListState,
}

impl PickerShell {
    /// Open a shell over the given source. Immediately refreshes with an
    /// empty query to populate initial rows.
    pub fn open(mut source: Box<dyn QuerySource>) -> Self {
        let rows = source.refresh("");
        let mut list_state = ListState::default();
        if !rows.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            source,
            query: MiniInput::new(),
            rows,
            list_state,
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
                if !shell.rows.is_empty() {
                    shell.list_state.select(Some(0));
                }
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

    // ── Key handling ───────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PickerAction::Cancel
            }
            KeyCode::Up => {
                self.select_prev();
                PickerAction::None
            }
            KeyCode::Down => {
                self.select_next();
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
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len();
        let i = self.list_state.selected().map_or(0, |i| (i + len - 1) % len);
        self.list_state.select(Some(i));
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + 1) % self.rows.len());
        self.list_state.select(Some(i));
    }

    fn accept_selected(&self) -> PickerAction {
        let Some(idx) = self.list_state.selected() else {
            return PickerAction::Cancel;
        };
        match self.source.accept(idx) {
            Some(a) => PickerAction::Accept(a),
            None => PickerAction::Cancel,
        }
    }

    /// Refresh rows from the source using the current query; preserve
    /// selection on the same logical row where possible.
    fn refilter(&mut self) {
        let prev_primary = self
            .list_state
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.primary.clone());
        self.rows = self.source.refresh(self.query.text());
        let new_idx = match prev_primary {
            Some(p) => self.rows.iter().position(|r| r.primary == p).or(Some(0)),
            None => (!self.rows.is_empty()).then_some(0),
        };
        self.list_state.select(if self.rows.is_empty() {
            None
        } else {
            new_idx
        });
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

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render above `anchor` (the InputBar's rect), clipped to `container`.
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect) {
        let query_mode_owned = self.source.query_mode() == QueryMode::OwnedByShell;
        let list_rows = self.rows.len().clamp(1, 8) as u16;
        let query_rows = if query_mode_owned { 1 } else { 0 };
        let inner_rows = list_rows + query_rows;
        let popup_height = inner_rows + 2; // +2 for block border

        let popup_width = (container.width / 2).clamp(30, container.width);
        let x = anchor
            .x
            .saturating_add(2)
            .min(container.x + container.width.saturating_sub(popup_width));
        let y = anchor.y.saturating_sub(popup_height);
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let title = format!(" {} ", self.source.title());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(title, Style::default().fg(Color::Cyan)));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut cursor_cell: Option<(u16, u16)> = None;

        let list_area = if query_mode_owned {
            // Render query line at top of inner area.
            let q_area = Rect::new(inner.x, inner.y, inner.width, 1);
            let prompt = "search: ";
            let prompt_len = prompt.len() as u16;
            let q_text = self.query.text();
            let line = Line::from(vec![
                Span::styled(prompt, Style::default().fg(Color::DarkGray)),
                Span::raw(q_text.to_string()),
            ]);
            frame.render_widget(Paragraph::new(line), q_area);
            // Cursor placement: prompt + cursor byte offset (fine for
            // monospace ASCII; multi-byte alignment is a Phase 2 polish).
            let cx = q_area.x + prompt_len + self.query.cursor() as u16;
            let cy = q_area.y;
            cursor_cell = Some((cx, cy));
            Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1)
        } else {
            inner
        };

        if self.rows.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                "No matches. Type to refine, Esc to dismiss.",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(p, list_area);
        } else {
            let items: Vec<ListItem> = self
                .rows
                .iter()
                .map(|r| {
                    let mut spans = Vec::with_capacity(4);
                    // Primary with atom-span styling.
                    if r.atoms.is_empty() {
                        spans.push(Span::styled(
                            r.primary.clone(),
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        let mut cursor = 0usize;
                        for &(a, b) in &r.atoms {
                            if a > cursor && a <= r.primary.len() {
                                spans.push(Span::styled(
                                    r.primary[cursor..a].to_string(),
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ));
                            }
                            let end = b.min(r.primary.len());
                            if end > a {
                                spans.push(Span::styled(
                                    r.primary[a..end].to_string(),
                                    Style::default()
                                        .fg(Color::LightBlue)
                                        .add_modifier(Modifier::UNDERLINED),
                                ));
                                cursor = end;
                            }
                        }
                        if cursor < r.primary.len() {
                            spans.push(Span::styled(
                                r.primary[cursor..].to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    if !r.secondary.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            r.secondary.clone(),
                            Style::default().fg(Color::White),
                        ));
                    }
                    if !r.tag.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            r.tag.clone(),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list =
                List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(list, list_area, &mut self.list_state);
        }

        if let Some((cx, cy)) = cursor_cell {
            if cx < popup_area.x + popup_area.width && cy < popup_area.y + popup_area.height {
                frame.set_cursor_position((cx, cy));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::query_source::{HistoryQuerySource, RetrievalAccept};
    use crate::input_history::{InputHistoryEntry, InputStateSnapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn mk(text: &str) -> InputHistoryEntry {
        InputHistoryEntry::new(InputStateSnapshot::from_text(text))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
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
    fn selection_survives_refilter_when_row_still_present() {
        // This is the "selection reset on every keystroke" fix.
        let src = HistoryQuerySource::new(vec![mk("apple pie"), mk("apple juice"), mk("banana")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Down)); // select row 1
        shell.handle_key(key(KeyCode::Char('a'))); // filter — all three still match "a"
        // Row 1 used to be "apple juice"; after fuzzy scoring it may still
        // be present. The contract is: if the previously-selected row's
        // primary text is still in the new row list, selection tracks it.
        let rows = shell.row_primaries();
        let idx = rows.iter().position(|p| p == "apple juice");
        assert_eq!(shell.selected_index(), idx);
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
    fn ctrl_c_returns_cancel_action() {
        let src = HistoryQuerySource::new(vec![mk("x")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(ctrl('c'));
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
    fn accept_on_empty_rows_returns_cancel() {
        let src = HistoryQuerySource::new(Vec::new());
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Enter));
        assert!(matches!(act, PickerAction::Cancel));
    }
}
