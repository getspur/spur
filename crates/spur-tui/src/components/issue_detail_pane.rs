use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

use spur_pm::{Comment, Issue};

pub struct IssueDetailPane {
    scroll_offset: u16,
}

impl IssueDetailPane {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_up_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self) {
        // Capped at a reasonable max; render() silently shows empty lines past body end.
        // A tighter cap would require knowing body line count + visible height at scroll time.
        self.scroll_offset = self.scroll_offset.saturating_add(1).min(500);
    }

    pub fn scroll_down_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(500);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn reset(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn render(&self, issue: &Issue, comments: &[Comment], frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Issue: {} ", issue.id))
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let show_comments_teaser = !comments.is_empty();

        // Split inner: top metadata section, body, footer
        let chunks = if show_comments_teaser {
            Layout::vertical([
                Constraint::Length(1), // Title line
                Constraint::Length(1), // Metadata line 1: status, priority, type, assignee
                Constraint::Length(1), // Metadata line 2: due date, blocked_by
                Constraint::Length(1), // Metadata line 3: labels
                Constraint::Length(1), // Separator
                Constraint::Min(1),    // Body (scrollable)
                Constraint::Length(1), // Footer separator
                Constraint::Length(1), // Comments teaser
                Constraint::Length(1), // Action hints
            ])
            .split(inner)
        } else {
            Layout::vertical([
                Constraint::Length(1), // Title line
                Constraint::Length(1), // Metadata line 1: status, priority, type, assignee
                Constraint::Length(1), // Metadata line 2: due date, blocked_by
                Constraint::Length(1), // Metadata line 3: labels
                Constraint::Length(1), // Separator
                Constraint::Min(1),    // Body (scrollable)
                Constraint::Length(1), // Footer separator
                Constraint::Length(1), // Action hints
            ])
            .split(inner)
        };

        // ── Title ────────────────────────────────────────────────────────────────
        let title_line = Line::from(Span::styled(
            issue.title.as_str(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(title_line), chunks[0]);

        // ── Metadata line 1: status · priority · type · assignee ─────────────────
        let status_span = status_colored_span(&issue.status);
        let priority_span = match issue.priority {
            Some(p) => Span::styled(format!("P{}", p), Style::default().fg(Color::Yellow)),
            None => Span::styled("--", Style::default().fg(Color::DarkGray)),
        };
        let type_span = Span::raw(issue.issue_type.as_deref().unwrap_or("--").to_string());
        let assignee_span = Span::styled(
            issue
                .assignee
                .as_deref()
                .unwrap_or("unassigned")
                .to_string(),
            Style::default().fg(Color::White),
        );

        let meta1 = Line::from(vec![
            Span::styled("status:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            status_span,
            Span::raw("  "),
            Span::styled("priority:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            priority_span,
            Span::raw("  "),
            Span::styled("type:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            type_span,
            Span::raw("  "),
            Span::styled("assignee:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            assignee_span,
        ]);
        frame.render_widget(Paragraph::new(meta1), chunks[1]);

        // ── Metadata line 2: due date · blocked_by ───────────────────────────────
        let due_str = issue
            .due_at
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "--".to_string());
        let mut meta2_spans = vec![
            Span::styled("due:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::raw(due_str),
        ];
        if !issue.blocked_by.is_empty() {
            meta2_spans.push(Span::raw("  "));
            meta2_spans.push(Span::styled(
                "blocked_by:",
                Style::default().fg(Color::DarkGray),
            ));
            meta2_spans.push(Span::raw(" "));
            meta2_spans.push(Span::styled(
                issue.blocked_by.join(", "),
                Style::default().fg(Color::Red),
            ));
        }
        let meta2 = Line::from(meta2_spans);
        frame.render_widget(Paragraph::new(meta2), chunks[2]);

        // ── Metadata line 3: labels ──────────────────────────────────────────────
        let labels_str = if issue.labels.is_empty() {
            "--".to_string()
        } else {
            issue.labels.join(", ")
        };
        let mut meta3_spans = vec![
            Span::styled("labels:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::raw(labels_str),
        ];
        if !issue.url.is_empty() {
            meta3_spans.push(Span::raw("  "));
            meta3_spans.push(Span::styled("url:", Style::default().fg(Color::DarkGray)));
            meta3_spans.push(Span::raw(" "));
            meta3_spans.push(Span::styled(
                issue.url.as_str(),
                Style::default().fg(Color::Cyan),
            ));
        }
        let meta3 = Line::from(meta3_spans);
        frame.render_widget(Paragraph::new(meta3), chunks[3]);

        // ── Separator ────────────────────────────────────────────────────────────
        let sep_width = inner.width as usize;
        let sep = "─".repeat(sep_width);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                sep,
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[4],
        );

        // ── Body (scrollable) ────────────────────────────────────────────────────
        let body_lines: Vec<Line> = issue
            .body
            .lines()
            .map(|l| Line::from(l.to_string()))
            .collect();
        let body_para = Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset, 0));
        frame.render_widget(body_para, chunks[5]);

        // ── Footer separator ─────────────────────────────────────────────────────
        let footer_sep = "─".repeat(inner.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                footer_sep,
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[6],
        );

        if show_comments_teaser {
            let reviews = comments.iter().filter(|c| is_review_comment(c)).count();
            let teaser_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM);
            let teaser = Line::from(vec![
                Span::styled(
                    format!("{} comments · {} reviews   ", comments.len(), reviews),
                    teaser_style,
                ),
                Span::styled("[c]", teaser_style),
                Span::styled("omments", teaser_style),
            ]);
            frame.render_widget(Paragraph::new(teaser), chunks[7]);
        }

        // ── Action hints ─────────────────────────────────────────────────────────
        let hints = Line::from(vec![
            Span::styled("[o]", Style::default().fg(Color::Green)),
            Span::styled("pen ", Style::default().fg(Color::DarkGray)),
            Span::styled("[w]", Style::default().fg(Color::Cyan)),
            Span::styled("ip ", Style::default().fg(Color::DarkGray)),
            Span::styled("[b]", Style::default().fg(Color::Red)),
            Span::styled("locked ", Style::default().fg(Color::DarkGray)),
            Span::styled("[d]", Style::default().fg(Color::DarkGray)),
            Span::styled("one  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[W]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("ork  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Esc]", Style::default().fg(Color::DarkGray)),
            Span::styled(" back", Style::default().fg(Color::DarkGray)),
        ]);
        let hints_idx = if show_comments_teaser { 8 } else { 7 };
        frame.render_widget(Paragraph::new(hints), chunks[hints_idx]);
    }

    pub fn render_loading(id: &str, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Issue: {} ", id))
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let msg = Paragraph::new(format!("Loading issue {}...", id))
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));

        // Center vertically
        let vert = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        frame.render_widget(msg, vert[1]);
    }
}

impl Default for IssueDetailPane {
    fn default() -> Self {
        Self::new()
    }
}

fn status_colored_span(status: &str) -> Span<'static> {
    match status {
        "open" => Span::styled("open".to_string(), Style::default().fg(Color::Green)),
        "in_progress" => Span::styled("wip".to_string(), Style::default().fg(Color::Cyan)),
        "blocked" => Span::styled("blk".to_string(), Style::default().fg(Color::Red)),
        "closed" => Span::styled("done".to_string(), Style::default().fg(Color::DarkGray)),
        other => Span::styled(other.to_string(), Style::default().fg(Color::White)),
    }
}

fn is_review_comment(comment: &Comment) -> bool {
    let actor = comment.actor.to_ascii_lowercase();
    let body = comment.body.to_ascii_lowercase();
    actor.contains("review") || body.starts_with("[review]") || body.contains("[[spur-review")
}
