use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use spur_core::{Artifact, ExecutorNode, WorkerStreamKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Stream,
    Artifacts,
    Attempts,
    Task,
    Review,
}

impl DetailTab {
    pub fn all() -> &'static [DetailTab] {
        &[
            DetailTab::Stream,
            DetailTab::Artifacts,
            DetailTab::Attempts,
            DetailTab::Task,
            DetailTab::Review,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Stream => "stream",
            DetailTab::Artifacts => "artifacts",
            DetailTab::Attempts => "attempts",
            DetailTab::Task => "task",
            DetailTab::Review => "review",
        }
    }
}

pub struct DetailPane {
    pub current_tab: DetailTab,
    scroll_offset: usize,
    is_following: bool,
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            current_tab: DetailTab::Stream,
            scroll_offset: 0,
            is_following: true,
        }
    }

    pub fn cycle_tab(&mut self, forward: bool) {
        let all = DetailTab::all();
        let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
        let next = if forward {
            (idx + 1) % all.len()
        } else {
            (idx + all.len() - 1) % all.len()
        };
        self.current_tab = all[next];
        self.scroll_offset = 0;
        self.is_following = true;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.is_following = true;
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, node: &ExecutorNode, issue_badge: Option<&str>) {
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", node.agent))
            .title_bottom(following_indicator);

        if let Some(badge) = issue_badge {
            block = block.title_top(
                Line::from(format!(" {} ", badge)).alignment(Alignment::Right),
            );
            block = block.title_bottom(
                Line::from(" [I]ssue detail ").alignment(Alignment::Right),
            );
        }

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);

        // Tab bar
        let titles: Vec<Line> = DetailTab::all()
            .iter()
            .map(|t| {
                let style = if *t == self.current_tab {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(t.label(), style))
            })
            .collect();
        let tabs = Tabs::new(titles)
            .select(
                DetailTab::all()
                    .iter()
                    .position(|t| *t == self.current_tab)
                    .unwrap_or(0),
            )
            .divider("│");
        frame.render_widget(tabs, chunks[0]);

        // Body
        let body_area = chunks[1];
        let visible_h = body_area.height as usize;

        let body_lines = match self.current_tab {
            DetailTab::Stream => self.render_stream(node, body_area.width),
            DetailTab::Artifacts => self.render_artifacts(node),
            DetailTab::Attempts => self.render_attempts(node),
            DetailTab::Task => self.render_task(node),
            DetailTab::Review => self.render_review(node),
        };

        let total = body_lines.len();
        let max_offset = total.saturating_sub(visible_h);
        if self.is_following {
            self.scroll_offset = max_offset;
        } else {
            self.scroll_offset = self.scroll_offset.min(max_offset);
            // Re-engage following when user scrolls to the bottom
            if self.scroll_offset >= max_offset && max_offset > 0 {
                self.is_following = true;
            }
        }

        let p = Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset as u16, 0));
        frame.render_widget(p, body_area);
    }

    fn render_stream(&self, node: &ExecutorNode, width: u16) -> Vec<Line<'static>> {
        if node.stream_buffer.is_empty() {
            return vec![Line::from(Span::styled(
                "(waiting for worker output…)",
                Style::default().fg(Color::DarkGray),
            ))];
        }

        // Coalesce consecutive same-kind Thought/Message chunks into blocks.
        // ToolCall entries are never merged — each is its own line.
        let mut blocks: Vec<(WorkerStreamKind, String, std::time::SystemTime)> = Vec::new();
        for entry in &node.stream_buffer {
            let text: String = entry
                .text
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();

            let should_merge = matches!(
                entry.kind,
                WorkerStreamKind::Thought | WorkerStreamKind::Message
            ) && blocks
                .last()
                .is_some_and(|(k, _, _)| *k == entry.kind);

            if should_merge {
                let last = blocks.last_mut().unwrap();
                last.1.push_str(&text);
                last.2 = entry.occurred_at; // update timestamp to latest chunk
            } else {
                blocks.push((entry.kind, text, entry.occurred_at));
            }
        }

        let now = std::time::SystemTime::now();
        let w = width as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut prev_kind: Option<WorkerStreamKind> = None;

        for (kind, text, occurred_at) in &blocks {
            // Separator on kind transition
            if let Some(pk) = prev_kind {
                if pk != *kind {
                    let sep: String = " ─"
                        .chars()
                        .chain(std::iter::repeat_n('─', w.saturating_sub(3)))
                        .collect();
                    lines.push(Line::from(Span::styled(
                        sep,
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            prev_kind = Some(*kind);

            let (prefix, style) = match kind {
                WorkerStreamKind::Thought => ("  · ", Style::default().fg(Color::DarkGray)),
                WorkerStreamKind::Message => ("  ▸ ", Style::default().fg(Color::White)),
                WorkerStreamKind::ToolCall => (
                    "  ▶ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            };

            let ago = now
                .duration_since(*occurred_at)
                .unwrap_or_default()
                .as_secs();
            let ts = if ago < 60 {
                format!("{}s", ago)
            } else {
                format!("{}m", ago / 60)
            };
            let ts_display = format!(" {}", ts);

            let prefix_cols = UnicodeWidthStr::width(prefix);
            let ts_cols = UnicodeWidthStr::width(ts_display.as_str());
            let text_budget = w.saturating_sub(prefix_cols + ts_cols + 1);

            let display_text = truncate_to_width(text, text_budget);
            let display_cols = UnicodeWidthStr::width(display_text.as_str());

            let pad = w.saturating_sub(prefix_cols + display_cols + ts_cols);
            let padding: String = " ".repeat(pad);

            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(display_text, style),
                Span::raw(padding),
                Span::styled(ts_display, Style::default().fg(Color::DarkGray)),
            ]));
        }

        lines
    }

    fn render_artifacts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        let mut out = Vec::new();
        for attempt in &node.attempts {
            for a in &attempt.artifacts {
                out.push(match a {
                    Artifact::Diff { summary, .. } => Line::from(format!(
                        "diff: {} files, +{} -{}",
                        summary.files_changed, summary.insertions, summary.deletions
                    )),
                    Artifact::PrUrl(u) => Line::from(format!("pr: {}", u)),
                    Artifact::FileList(f) => Line::from(format!("files: {}", f.len())),
                    Artifact::Text(t) => Line::from(t.clone()),
                });
            }
        }
        // Render full diff text when available.
        if let Some(ref diff_text) = node.latest_diff_text {
            if !diff_text.is_empty() {
                out.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
                out.extend(super::diff_viewer::render_diff_lines(diff_text));
            }
        }
        if out.is_empty() {
            out.push(Line::from(Span::styled(
                "(no artifacts yet)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        out
    }

    fn render_attempts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        node.attempts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                Line::from(format!(
                    "#{}: {:?}  cost=${:.2}  session={}",
                    i + 1,
                    a.status,
                    a.cost_usd,
                    a.session_id.0
                ))
            })
            .collect()
    }

    fn render_task<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        if node.task_spec.is_empty() {
            vec![Line::from(Span::styled(
                "(no task spec captured)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            node.task_spec
                .lines()
                .map(|l| Line::from(l.to_string()))
                .collect()
        }
    }

    fn render_review(&self, node: &ExecutorNode) -> Vec<Line<'static>> {
        super::review_card::render_review(node)
    }
}

/// Truncate a string to fit within `max_cols` display columns (UTF-8 safe).
/// Appends '…' if truncated.
fn truncate_to_width(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let full_width = UnicodeWidthStr::width(s);
    if full_width <= max_cols {
        return s.to_string();
    }
    // Need to truncate — reserve 1 col for '…'
    let target = max_cols.saturating_sub(1);
    let mut cols = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + cw > target {
            break;
        }
        cols += cw;
        end = i + ch.len_utf8();
    }
    format!("{}…", &s[..end])
}
