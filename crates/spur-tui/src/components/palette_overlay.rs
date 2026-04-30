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
    session_active: bool,
}

impl<'a> PaletteOverlay<'a> {
    pub fn new(state: &'a PaletteState) -> Self {
        Self {
            state,
            session_active: false,
        }
    }

    pub fn with_session_active(mut self, active: bool) -> Self {
        self.session_active = active;
        self
    }

    fn render_flat(&self, area: Rect, buf: &mut Buffer) {
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
                    Span::styled(r.subtitle.clone(), Style::default().fg(Color::DarkGray)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect();
        List::new(items).render(area, buf);
    }

    fn render_grouped(&self, area: Rect, buf: &mut Buffer) {
        use crate::components::palette::PaletteResult;

        // Partition ranked results by kind. Track each row's global index
        // (its position in iter_ranked) so we can apply the REVERSED
        // selection highlight to the row at state.cursor().
        let mut views: Vec<(usize, &PaletteResult)> = Vec::new();
        let mut commands: Vec<(usize, &PaletteResult)> = Vec::new();
        let mut sessions: Vec<(usize, &PaletteResult)> = Vec::new();
        let mut workers: Vec<(usize, &PaletteResult)> = Vec::new();
        for (i, r) in self.state.iter_ranked().enumerate() {
            match r.kind {
                PaletteKind::View => views.push((i, r)),
                PaletteKind::Command => commands.push((i, r)),
                PaletteKind::Session => sessions.push((i, r)),
                PaletteKind::Worker => workers.push((i, r)),
                PaletteKind::Trace => { /* skipped upstream */ }
            }
        }

        // Per-kind cap — auto-scales by available height. Reserve TRACE_ROW
        // up front so the placeholder always renders at the bottom of the
        // grouped view (it's the discoverability anchor for the deferred
        // feature; see U3c).
        //
        // Spec aspirationally says "minimum 2 per kind", but at 80x24 with
        // three populated kinds (3 headers + 6 rows + 1 trace = 10 rows)
        // that floor would push past the typical 9-row list area and force
        // the TRACE placeholder out. We chose TRACE visibility over min-2:
        // at tight terminals the cap can drop to 1 (or 0) per kind, which
        // is acceptable degradation. Headers for empty kinds are skipped
        // entirely (see `if rows.is_empty() { return }` in render_section).
        const TRACE_ROW: u16 = 1;
        const PER_KIND_MAX: u16 = 5;
        let kinds_with_data: u16 = [
            views.is_empty(),
            commands.is_empty(),
            sessions.is_empty(),
            workers.is_empty(),
        ]
        .iter()
        .filter(|empty| !*empty)
        .count() as u16;

        let header_rows = kinds_with_data;
        let available_for_data = area.height.saturating_sub(TRACE_ROW + header_rows);
        let cap: usize = if kinds_with_data == 0 {
            0
        } else {
            ((available_for_data / kinds_with_data).min(PER_KIND_MAX)) as usize
        };

        let cursor = self.state.cursor();
        let mut y = area.y;

        macro_rules! render_section {
            ($title:expr, $rows:expr) => {
                if $rows.is_empty() {
                    // nothing to do
                } else if cap == 0 {
                    return; // Skip the header entirely if no rows would fit.
                } else if y < area.y + area.height {
                    // Section header
                    Paragraph::new(Line::from(Span::styled(
                        $title.to_string(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )))
                    .render(
                        Rect {
                            x: area.x,
                            y,
                            width: area.width,
                            height: 1,
                        },
                        buf,
                    );
                    y = y.saturating_add(1);

                    for (global_idx, r) in $rows.iter().take(cap) {
                        if y >= area.y + area.height {
                            break;
                        }
                        let label_style = if *global_idx == cursor {
                            Style::default().add_modifier(Modifier::REVERSED)
                        } else {
                            Style::default()
                        };
                        let spans = vec![
                            Span::styled(
                                format!("  {}  ", badge_for(&r.kind)),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::styled(r.label.clone(), label_style),
                            Span::raw("   "),
                            Span::styled(r.subtitle.clone(), Style::default().fg(Color::DarkGray)),
                        ];
                        Paragraph::new(Line::from(spans)).render(
                            Rect {
                                x: area.x,
                                y,
                                width: area.width,
                                height: 1,
                            },
                            buf,
                        );
                        y = y.saturating_add(1);
                    }
                }
            };
        }

        render_section!("VIEWS", views);
        render_section!("COMMANDS", commands);
        render_section!("SESSIONS", sessions);
        render_section!("WORKERS", workers);

        // TRACE placeholder — discoverability anchor for the deferred
        // feature. The TRACE_ROW reservation above guarantees there is room
        // for this line at all reasonable terminal sizes.
        if y < area.y + area.height {
            Paragraph::new(Line::from(Span::styled(
                "TRACE \u{2014} coming soon",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )))
            .render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

fn badge_for(kind: &PaletteKind) -> &'static str {
    match kind {
        PaletteKind::View => "@",
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
    Rect {
        x,
        y,
        width: w.min(outer.width),
        height: h.min(outer.height),
    }
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

        if inner.height < 3 || inner.width < 10 {
            return;
        }

        // Layout: row 0 = query; row 1 = blank; rows 2..=h-2 = results; last row = hints.
        let query_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
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
            let msg: &str = if self.state.query().is_empty() {
                "type to filter"
            } else if self.state.query().starts_with('/') && !self.session_active {
                "Slash commands need an active session."
            } else {
                "No matches. Try shorter or different keywords."
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )))
            .render(list_area, buf);
        } else if self.state.query().is_empty() {
            self.render_grouped(list_area, buf);
        } else {
            self.render_flat(list_area, buf);
        }

        // Hint line.
        let hint = Line::from(Span::styled(
            "↑↓ select   ⏎ accept   esc dismiss",
            Style::default().fg(Color::DarkGray),
        ));
        Paragraph::new(hint).render(hints_area, buf);
    }
}
