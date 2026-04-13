use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

/// A single row shown in the popup.
#[derive(Debug, Clone)]
pub struct PopupRow {
    /// Left label (e.g. "/help" or "@src/foo.rs").
    pub label: String,
    /// Middle description (may be empty).
    pub description: String,
    /// Right-side tag (e.g. "⟨claude⟩"). Empty string for no tag.
    pub source_tag: String,
}

/// Overlay list widget shown above the InputBar for autocomplete.
pub struct CompletionPopup {
    rows: Vec<PopupRow>,
    state: ListState,
    empty_message: String,
    /// Cached row widths recomputed on `set_rows`. Avoids 3× per-frame scan.
    max_label: usize,
    max_desc: usize,
    max_tag: usize,
}

impl CompletionPopup {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            state: ListState::default(),
            empty_message: "No matches. Type to refine, Esc to dismiss.".to_string(),
            max_label: 0,
            max_desc: 0,
            max_tag: 0,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<PopupRow>) {
        self.max_label = rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
        self.max_desc = rows.iter().map(|r| r.description.len()).max().unwrap_or(0);
        self.max_tag = rows.iter().map(|r| r.source_tag.len()).max().unwrap_or(0);
        self.rows = rows;
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        } else {
            self.state.select(None);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[PopupRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self
            .state
            .selected()
            .map_or(0, |i| (i + 1) % self.rows.len());
        self.state.select(Some(i));
    }

    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len();
        let i = self.state.selected().map_or(0, |i| (i + len - 1) % len);
        self.state.select(Some(i));
    }

    /// Render above `anchor` (typically the InputBar's rect), clipped to
    /// `container`.
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect) {
        let max_rows = self.rows.len().clamp(1, 8) as u16;
        let popup_height = max_rows + 2; // +2 for the block border
        let desired_width = (self.max_label + self.max_desc + self.max_tag + 8) as u16;
        let popup_width = desired_width
            .min(container.width / 2)
            .max(30)
            .min(container.width);

        let x = anchor
            .x
            .saturating_add(2)
            .min(container.x + container.width.saturating_sub(popup_width));
        let y = anchor.y.saturating_sub(popup_height);
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        if self.rows.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                self.empty_message.as_str(),
                Style::default().fg(Color::DarkGray),
            )))
            .block(block);
            frame.render_widget(p, popup_area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let mut spans = Vec::with_capacity(4);
                spans.push(Span::styled(
                    r.label.clone(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
                if !r.description.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.description.clone(),
                        Style::default().fg(Color::White),
                    ));
                }
                if !r.source_tag.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.source_tag.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(list, popup_area, &mut self.state);
    }
}

impl Default for CompletionPopup {
    fn default() -> Self {
        Self::new()
    }
}
