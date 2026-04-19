//! Palette modal overlay — ratatui widget.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget},
};

use crate::components::palette::{PaletteKind, PaletteState};

pub struct PaletteOverlay<'a> {
    state: &'a PaletteState,
}

impl<'a> PaletteOverlay<'a> {
    pub fn new(state: &'a PaletteState) -> Self {
        Self { state }
    }
}

fn badge_for(kind: &PaletteKind) -> &'static str {
    match kind {
        PaletteKind::Command => ">",
        PaletteKind::Session => "$",
        PaletteKind::Worker => "!",
        PaletteKind::Trace => "#",
    }
}

fn modal_rect(outer: Rect) -> Rect {
    // Centered modal: 60% width, 60% height, min 40x8.
    let w = (outer.width as u32 * 6 / 10).max(40) as u16;
    let h = (outer.height as u32 * 6 / 10).max(8) as u16;
    let x = outer.x + (outer.width.saturating_sub(w)) / 2;
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w.min(outer.width), height: h.min(outer.height) }
}

impl<'a> Widget for PaletteOverlay<'a> {
    fn render(self, outer: Rect, buf: &mut Buffer) {
        let area = modal_rect(outer);
        Clear.render(area, buf); // blank the modal area

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Go to…  (Ctrl+K) ")
            .title_alignment(Alignment::Left);
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 3 || inner.width < 10 { return; }

        // Layout: row 0 = query; row 1 = blank; rows 2..=h-2 = results; last row = hints.
        let query_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 };
        let hints_area = Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        };
        let list_area = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height.saturating_sub(3),
        };

        // Query line: "> refac▮"
        let query_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::raw(self.state.query()),
            Span::styled("▮", Style::default().fg(Color::Gray)),
        ]);
        Paragraph::new(query_line).render(query_area, buf);

        // Results or empty-state placeholder.
        if self.state.ranked_len() == 0 {
            let msg = if self.state.query().is_empty() {
                "type to filter"
            } else {
                "no matches"
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )))
            .render(list_area, buf);
        } else {
            let items: Vec<ListItem> = self
                .state
                .iter_ranked()
                .enumerate()
                .map(|(i, r)| {
                    let selected = i == self.state.cursor();
                    let style = if selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    let spans = vec![
                        Span::styled(
                            format!("  {}  ", badge_for(&r.kind)),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(r.label.clone(), style),
                        Span::raw("   "),
                        Span::styled(
                            r.subtitle.clone(),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ];
                    ListItem::new(Line::from(spans))
                })
                .collect();
            List::new(items).render(list_area, buf);
        }

        // Hint line.
        let hint = Line::from(Span::styled(
            "↑↓ select · ↵ go · esc close · type to filter",
            Style::default().fg(Color::DarkGray),
        ));
        Paragraph::new(hint).render(hints_area, buf);
    }
}
