use chrono::{DateTime, Duration, Utc};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use spur_pm::Comment;
use unicode_width::UnicodeWidthStr;

use crate::components::line_wrap::wrap_line_to_width;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentFilter {
    All,
    Human,
    Review,
    Machine,
}

impl CommentFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Human,
            Self::Human => Self::Review,
            Self::Review => Self::Machine,
            Self::Machine => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Human => "human",
            Self::Review => "review",
            Self::Machine => "machine",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentKind {
    Human,
    Review,
    Audit,
    Signal,
}

impl CommentKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Review => "review",
            Self::Audit => "audit",
            Self::Signal => "signal",
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedComment<'a> {
    comment: &'a Comment,
    kind: CommentKind,
}

pub struct IssueCommentsPane {
    scroll_offset: u16,
    filter: CommentFilter,
    last_body_height: u16,
    last_total_lines: usize,
    last_review_offsets: Vec<usize>,
}

impl IssueCommentsPane {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            filter: CommentFilter::All,
            last_body_height: 0,
            last_total_lines: 0,
            last_review_offsets: Vec::new(),
        }
    }

    pub fn scroll_up_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(500);
    }

    pub fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.scroll_offset = 0;
        self.last_body_height = 0;
        self.last_total_lines = 0;
        self.last_review_offsets.clear();
    }

    pub fn jump_to_bottom(&mut self) {
        let max_scroll = self
            .last_total_lines
            .saturating_sub(self.last_body_height as usize);
        self.scroll_offset = max_scroll.min(u16::MAX as usize) as u16;
    }

    pub fn jump_to_next_review(&mut self) {
        if self.last_review_offsets.is_empty() {
            return;
        }
        let current = self.scroll_offset as usize;
        let next = self
            .last_review_offsets
            .iter()
            .copied()
            .find(|line| *line > current)
            .unwrap_or(self.last_review_offsets[0]);
        self.scroll_offset = next.min(u16::MAX as usize) as u16;
    }

    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    pub fn set_scroll_offset(&mut self, scroll_offset: u16) {
        self.scroll_offset = scroll_offset;
    }

    pub fn render(&mut self, id: &str, comments: &[Comment], frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Comments: {} ", id))
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        self.last_body_height = chunks[0].height;

        if comments.is_empty() {
            self.last_total_lines = 1;
            self.last_review_offsets.clear();
            let empty = Paragraph::new("No comments yet")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(empty, chunks[0]);
        } else {
            let prepared = self.prepare_comments(comments);
            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut review_offsets: Vec<usize> = Vec::new();
            let body_width = chunks[0].width.max(1) as usize;

            for (idx, comment) in prepared.iter().enumerate() {
                let start_line = lines.len();
                if comment.kind == CommentKind::Review {
                    review_offsets.push(start_line);
                }
                self.push_comment_lines(comment, body_width, &mut lines);
                if idx + 1 < prepared.len() {
                    lines.push(Line::from(Span::styled(
                        " ",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }

            self.last_total_lines = lines.len();
            self.last_review_offsets = review_offsets;
            let max_scroll = self
                .last_total_lines
                .saturating_sub(self.last_body_height as usize);
            if self.scroll_offset as usize > max_scroll {
                self.scroll_offset = max_scroll.min(u16::MAX as usize) as u16;
            }

            let paragraph = Paragraph::new(lines).scroll((self.scroll_offset, 0));
            frame.render_widget(paragraph, chunks[0]);
        }

        let footer = Line::from(vec![
            Span::styled("filter:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(
                self.filter.label(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  [f] cycle  [r] next review  [G] bottom",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[1]);
    }

    fn prepare_comments<'a>(&self, comments: &'a [Comment]) -> Vec<PreparedComment<'a>> {
        let mut items: Vec<PreparedComment<'a>> = comments
            .iter()
            .map(|comment| PreparedComment {
                comment,
                kind: classify_comment(comment),
            })
            .collect();

        items.sort_by_key(|entry| entry.comment.created_at);

        let cutoff = Utc::now() - Duration::hours(24);
        let mut recent_reviews = Vec::new();
        let mut rest = Vec::new();
        for entry in items {
            if entry.kind == CommentKind::Review && entry.comment.created_at >= cutoff {
                recent_reviews.push(entry);
            } else {
                rest.push(entry);
            }
        }
        recent_reviews.extend(rest);

        recent_reviews
            .into_iter()
            .filter(|entry| match self.filter {
                CommentFilter::All => true,
                CommentFilter::Human => entry.kind == CommentKind::Human,
                CommentFilter::Review => entry.kind == CommentKind::Review,
                CommentFilter::Machine => {
                    matches!(entry.kind, CommentKind::Audit | CommentKind::Signal)
                }
            })
            .collect()
    }

    fn push_comment_lines(
        &self,
        comment: &PreparedComment<'_>,
        width: usize,
        out: &mut Vec<Line<'static>>,
    ) {
        let (glyph, base_style, actor_style, body_style, tag_style, review_bar) =
            style_for_kind(comment.kind);
        let actor = comment.comment.actor.as_str();
        let timestamp = relative_time(comment.comment.created_at);
        let kind_tag = format!("[{}]", comment.kind.tag());
        let left_text = format!("{glyph} {actor} {timestamp}");
        let left_prefix = if review_bar { "│ " } else { "" };
        let left_width =
            UnicodeWidthStr::width(left_prefix) + UnicodeWidthStr::width(left_text.as_str());
        let tag_width = UnicodeWidthStr::width(kind_tag.as_str());
        let spacing = width.saturating_sub(left_width + tag_width).max(1);

        let mut header_spans = vec![
            Span::styled(format!("{glyph} "), base_style),
            Span::styled(actor.to_string(), actor_style),
            Span::raw(" "),
            Span::styled(
                timestamp,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::raw(" ".repeat(spacing)),
            Span::styled(kind_tag, tag_style),
        ];
        if review_bar {
            header_spans.insert(0, Span::styled("│ ", Style::default().fg(Color::Yellow)));
        }
        out.push(Line::from(header_spans));

        let indent = if review_bar { "│   " } else { "    " };
        let wrapped_width = width.saturating_sub(UnicodeWidthStr::width(indent)).max(1) as u16;
        for raw_line in comment.comment.body.lines() {
            let wrapped = wrap_line_to_width(
                &Line::from(Span::styled(raw_line.to_string(), body_style)),
                wrapped_width,
            );
            for wrapped_line in wrapped {
                out.push(Line::from(vec![
                    Span::styled(indent.to_string(), base_style),
                    Span::styled(wrapped_line.to_string(), body_style),
                ]));
            }
        }
    }
}

impl Default for IssueCommentsPane {
    fn default() -> Self {
        Self::new()
    }
}

fn classify_comment(comment: &Comment) -> CommentKind {
    let actor = comment.actor.to_ascii_lowercase();
    let body = comment.body.to_ascii_lowercase();

    let review_actor = actor.contains("review");
    let review_body = body.starts_with("[review]") || body.contains("[[spur-review");
    if review_actor || review_body {
        return CommentKind::Review;
    }

    if actor == "spur-bot" || body.contains("[spur-audit") {
        return CommentKind::Audit;
    }

    if comment.body.trim_start().starts_with('⚡') || body.contains("signal:") {
        return CommentKind::Signal;
    }

    CommentKind::Human
}

fn style_for_kind(kind: CommentKind) -> (&'static str, Style, Style, Style, Style, bool) {
    match kind {
        CommentKind::Review => (
            "█",
            Style::default().fg(Color::Yellow),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
            Style::default().fg(Color::Yellow),
            true,
        ),
        CommentKind::Audit => (
            "▸",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM | Modifier::ITALIC),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            false,
        ),
        CommentKind::Signal => (
            "▸",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            Style::default().fg(Color::White),
            Style::default().fg(Color::White),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            false,
        ),
        CommentKind::Human => (
            "├─",
            Style::default().fg(Color::White),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
            Style::default().fg(Color::White),
            false,
        ),
    }
}

fn relative_time(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now.signed_duration_since(time);
    if delta < Duration::minutes(1) {
        "now".to_string()
    } else if delta < Duration::hours(1) {
        format!("{}m ago", delta.num_minutes())
    } else if delta < Duration::days(1) {
        format!("{}h ago", delta.num_hours())
    } else {
        format!("{}d ago", delta.num_days())
    }
}
