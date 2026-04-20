use std::borrow::Cow;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};
use spur_core::{Artifact, ExecutorNode};

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

impl Default for DetailPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the bottom-border scroll label for a DetailPane state.
///
/// Pure function — no ratatui dependencies, no borrow of `DetailPane`.
/// Exhaustive state coverage per the design spec's state table.
///
/// Arguments:
///   - `tab`: current tab
///   - `total`: number of wrapped body rows (0 if empty/placeholder)
///   - `visible_h`: viewport height in rows
///   - `scroll_offset`: current scroll offset in rows
///   - `is_following`: pane's own follow flag (authoritative for non-Stream)
///   - `stream_trace_following`: Some(trace.is_following()) when on Stream
///     with a trace present; None when on Stream placeholder or on a
///     non-Stream tab.
fn scroll_label(
    tab: DetailTab,
    total: usize,
    visible_h: usize,
    scroll_offset: usize,
    is_following: bool,
    stream_trace_following: Option<bool>,
) -> Cow<'static, str> {
    // Stream tab — follow flag comes from the trace (if present).
    if matches!(tab, DetailTab::Stream) {
        return match stream_trace_following {
            Some(true) => Cow::Borrowed(" ▼ following "),
            Some(false) => Cow::Borrowed(" ▲ paused "),
            // No trace yet — placeholder path. Default to "following"
            // so the initial render does not look stalled.
            None => Cow::Borrowed(" ▼ following "),
        };
    }

    // Non-Stream tabs — authoritative scroll + follow state on DetailPane.
    if total == 0 {
        return Cow::Borrowed("");
    }
    let max_offset = total.saturating_sub(visible_h);
    if max_offset == 0 {
        // Content fits viewport; nothing to scroll.
        return Cow::Borrowed(" ▼ ");
    }
    if is_following {
        return Cow::Borrowed(" ▼ ");
    }
    if scroll_offset == 0 {
        return Cow::Borrowed(" top ");
    }
    if scroll_offset >= max_offset {
        return Cow::Borrowed(" end ");
    }
    Cow::Owned(format!(" ▲ {} ↑ ", scroll_offset))
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            current_tab: DetailTab::Stream,
            scroll_offset: 0,
            is_following: true,
        }
    }

    /// Cycle to the next (or previous) tab.
    ///
    /// Intent is split by tab kind:
    /// - **Stream**: snap the trace to latest (`trace.scroll_to_bottom()`)
    ///   when the trace is available, and set the local follow flag for
    ///   API symmetry. The badge is actually derived from `trace.is_following()`
    ///   inside `render`, so the local flag is advisory on this tab.
    /// - **Non-Stream**: open at the top — `scroll_offset = 0`,
    ///   `is_following = false`. Previously both `is_following = true` and
    ///   `scroll_offset = 0` were set, which the render re-clamped to
    ///   `max_offset` (the bottom), inverting the user's expected "fresh
    ///   tab" UX.
    pub fn cycle_tab(
        &mut self,
        forward: bool,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        let all = DetailTab::all();
        let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
        let next = if forward {
            (idx + 1) % all.len()
        } else {
            (idx + all.len() - 1) % all.len()
        };
        self.current_tab = all[next];
        self.scroll_offset = 0;
        match self.current_tab {
            DetailTab::Stream => {
                self.is_following = true;
                if let Some(t) = stream_trace {
                    t.scroll_to_bottom();
                }
            }
            _ => {
                self.is_following = false;
            }
        }
    }

    pub fn scroll_up(
        &mut self,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        if matches!(self.current_tab, DetailTab::Stream) {
            if let Some(trace) = stream_trace {
                trace.scroll_up();
                return;
            }
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_down(
        &mut self,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        if matches!(self.current_tab, DetailTab::Stream) {
            if let Some(trace) = stream_trace {
                trace.scroll_down();
                return;
            }
        }
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_to_top(
        &mut self,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        if matches!(self.current_tab, DetailTab::Stream) {
            if let Some(trace) = stream_trace {
                trace.scroll_to_top();
                return;
            }
        }
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(
        &mut self,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        if matches!(self.current_tab, DetailTab::Stream) {
            if let Some(trace) = stream_trace {
                trace.scroll_to_bottom();
                return;
            }
        }
        self.is_following = true;
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        node: &ExecutorNode,
        issue_badge: Option<&str>,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        // Source-of-truth for the follow badge:
        // - Stream tab with a live trace: the trace's own anchor. This is
        //   the authoritative viewport state for the ReactTrace widget.
        //   Without this, the pane's local flag would desync from the
        //   trace's anchor after a user-driven scroll.
        // - Stream tab without a trace (placeholder path): default to
        //   "following" so the initial render doesn't look stalled.
        // - Non-Stream tabs: the pane's own local flag, which tracks the
        //   per-tab bottom-follow behavior.
        let following = match self.current_tab {
            DetailTab::Stream => stream_trace
                .as_deref()
                .map(|t| t.is_following())
                .unwrap_or(true),
            _ => self.is_following,
        };
        let following_indicator = if following { " ▼ following " } else { "" };

        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", node.agent))
            .title_bottom(following_indicator);

        if let Some(badge) = issue_badge {
            block = block.title_top(Line::from(format!(" {} ", badge)).alignment(Alignment::Right));
            block = block.title_bottom(Line::from(" [I]ssue detail ").alignment(Alignment::Right));
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

        // For the Stream tab, delegate to ReactTrace when available.
        if self.current_tab == DetailTab::Stream {
            if let Some(trace) = stream_trace {
                // Delegate to the compact ReactTrace body renderer.
                // `render_compact` paints ONLY the body — DetailPane
                // owns the outer block. Using `ReactTrace::render`
                // here would double-draw borders.
                trace.render_compact(frame, body_area);
                // Scroll state lives on trace.anchor, not DetailPane's
                // scroll_offset. Other tabs still use scroll_offset.
                return;
            }
        }

        let body_lines = match self.current_tab {
            DetailTab::Stream => {
                // No trace materialized yet (orphan event or first-load race).
                // Placeholder; production code paths always produce a trace
                // via App::handle_spur_event.
                vec![Line::from(Span::styled(
                    "(no stream yet)",
                    Style::default().fg(Color::DarkGray),
                ))]
            }
            DetailTab::Artifacts => self.render_artifacts(node),
            DetailTab::Attempts => self.render_attempts(node),
            DetailTab::Task => self.render_task(node),
            DetailTab::Review => self.render_review(node),
        };

        // Pre-wrap at the body width so `max_offset` reflects the actual
        // number of rendered rows. Previously `Paragraph::wrap` wrapped at
        // render time while the ceiling was computed from unwrapped
        // `body_lines.len()`, which clipped scroll above the true bottom
        // on long single-line content (e.g., a 500-char task spec).
        let wrapped: Vec<Line<'static>> = body_lines
            .iter()
            .flat_map(|l| {
                crate::components::line_wrap::wrap_line_to_width(l, body_area.width)
            })
            .collect();
        let total = wrapped.len();
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

        // Input is already wrapped; don't re-wrap inside Paragraph.
        let p = Paragraph::new(wrapped).scroll((self.scroll_offset as u16, 0));
        frame.render_widget(p, body_area);
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

#[cfg(test)]
mod scroll_label_tests {
    use super::*;

    #[test]
    fn stream_with_trace_following_shows_following() {
        let s = scroll_label(DetailTab::Stream, 0, 0, 0, false, Some(true));
        assert_eq!(s, Cow::Borrowed(" ▼ following "));
    }

    #[test]
    fn stream_with_trace_paused_shows_paused() {
        let s = scroll_label(DetailTab::Stream, 0, 0, 0, false, Some(false));
        assert_eq!(s, Cow::Borrowed(" ▲ paused "));
    }

    #[test]
    fn stream_without_trace_shows_following() {
        // Placeholder path — no trace yet but pane wants to look live.
        let s = scroll_label(DetailTab::Stream, 1, 10, 0, true, None);
        assert_eq!(s, Cow::Borrowed(" ▼ following "));
    }

    #[test]
    fn non_stream_empty_total_shows_blank() {
        let s = scroll_label(DetailTab::Artifacts, 0, 20, 0, false, None);
        assert_eq!(s, Cow::Borrowed(""));
    }

    #[test]
    fn non_stream_content_fits_viewport_shows_down() {
        // total=10, visible=20 → max_offset=0 → content fits.
        let s = scroll_label(DetailTab::Task, 10, 20, 0, false, None);
        assert_eq!(s, Cow::Borrowed(" ▼ "));
    }

    #[test]
    fn non_stream_at_end_following_shows_down() {
        // total=100, visible=20, offset=80, following → " ▼ ".
        let s = scroll_label(DetailTab::Attempts, 100, 20, 80, true, None);
        assert_eq!(s, Cow::Borrowed(" ▼ "));
    }

    #[test]
    fn non_stream_at_top_shows_top() {
        // total=100, visible=20, offset=0, not following → " top ".
        let s = scroll_label(DetailTab::Review, 100, 20, 0, false, None);
        assert_eq!(s, Cow::Borrowed(" top "));
    }

    #[test]
    fn non_stream_at_end_not_following_shows_end() {
        // total=100, visible=20, offset=80 (= max_offset), not following.
        let s = scroll_label(DetailTab::Artifacts, 100, 20, 80, false, None);
        assert_eq!(s, Cow::Borrowed(" end "));
    }

    #[test]
    fn non_stream_mid_scroll_shows_arrow_count() {
        // total=100, visible=20, offset=42 → " ▲ 42 ↑ ".
        let s = scroll_label(DetailTab::Artifacts, 100, 20, 42, false, None);
        assert_eq!(s, Cow::<'static, str>::Owned(" ▲ 42 ↑ ".to_string()));
    }
}
