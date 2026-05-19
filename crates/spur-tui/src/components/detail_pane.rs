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
    pub(crate) current_tab: DetailTab,
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
) -> Cow<'static, str> {
    if total == 0 {
        return Cow::Borrowed("");
    }
    let max_offset = total.saturating_sub(visible_h);
    if max_offset == 0 {
        return Cow::Borrowed(" ▼ ");
    }
    if is_following {
        if matches!(tab, DetailTab::Stream) {
            return Cow::Borrowed(" ▼ following ");
        }
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

fn position_indicator(total: usize, visible: usize, offset: usize, width: u16) -> Option<String> {
    if total <= visible || width < 20 {
        return None;
    }

    let bottom = (offset + visible).min(total);
    let percent = bottom * 100 / total;

    if width < 30 {
        Some(format!(" · {percent}% "))
    } else {
        Some(format!(" · {bottom}/{total} · {percent}% "))
    }
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            current_tab: DetailTab::Stream,
            scroll_offset: 0,
            is_following: true,
        }
    }

    /// Read-only accessor for the current tab. External callers cannot
    /// write `current_tab` directly; use [`DetailPane::jump_to_tab`] or
    /// [`DetailPane::cycle_tab`] to change it.
    pub fn current_tab(&self) -> DetailTab {
        self.current_tab
    }

    /// Test-only accessor for the follow flag.
    #[doc(hidden)]
    pub fn is_following(&self) -> bool {
        self.is_following
    }

    /// Private shared helper. Encodes the per-tab reset invariants so
    /// every entry point (`cycle_tab`, `jump_to_tab`) cannot accidentally
    /// diverge. Opens at top (`scroll_offset = 0`) on every tab and sets
    /// `is_following` based on the destination tab kind.
    fn set_tab(&mut self, tab: DetailTab) {
        self.current_tab = tab;
        self.scroll_offset = 0;
        self.is_following = matches!(tab, DetailTab::Stream);
    }

    /// Cycle to the next (or previous) tab.
    ///
    /// Per-tab reset invariants (`scroll_offset = 0`, `is_following`
    /// per tab kind) are centralised in [`DetailPane::set_tab`].
    pub fn cycle_tab(&mut self, forward: bool) {
        let all = DetailTab::all();
        let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
        let next = if forward {
            (idx + 1) % all.len()
        } else {
            (idx + all.len() - 1) % all.len()
        };
        self.set_tab(all[next]);
    }

    /// Jump directly to a specific tab. Applies the same per-tab reset
    /// invariants as [`DetailPane::cycle_tab`] (scroll to top, set
    /// `is_following` per tab kind).
    ///
    /// Use this from outside the pane instead of writing `current_tab`
    /// directly — the field is `pub(crate)` and only readable via
    /// [`DetailPane::current_tab`].
    pub fn jump_to_tab(&mut self, tab: DetailTab) {
        self.set_tab(tab);
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.is_following = false;
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
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
        // ── 1. Skeleton block — shape-equivalent to the final block so
        //       Block::inner() returns the same rect. ──────────────────
        //
        // Every title POSITION that will appear on the final block must
        // also appear on the skeleton. Content can be placeholder because
        // inner() is a function of borders + title presence, not content.
        let mut skeleton = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .title(" ") // matches final top-left (agent name)
            .title_bottom(" "); // matches final bottom-left (scroll_label)
        if issue_badge.is_some() {
            skeleton = skeleton
                .title_top(Line::from(" ").alignment(Alignment::Right)) // matches final top-right (badge)
                .title_bottom(Line::from(" ").alignment(Alignment::Right)); // matches final bottom-right ([I]ssue)
        }
        let inner = skeleton.inner(area);
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        let body_area = chunks[1];

        // ── 2. Render tabs (shared across all tabs). ─────────────────
        let tabs_widget = self.build_tabs_widget();

        // ── 3. Per-tab body rendering. ───────────────────────────────
        //
        // The Stream tab delegates to the reusable `stream_pane::render_stream`
        // so the future plan_inspector peek overlay can reuse the same
        // renderer with its own independent `StreamViewState`. We sync
        // `self.scroll_offset` / `self.is_following` into a transient
        // `StreamViewState` and copy back the post-clamp values.
        //
        // Non-Stream tabs continue to render inline because they depend on
        // private `render_*` helpers that need `&ExecutorNode`.
        match self.current_tab {
            DetailTab::Stream => {
                // Layout: tabs row at the top, then the stream pane (which
                // owns its own borders) consumes the remainder. The body
                // area inside render_stream is `area.height - 1 (tabs) -
                // 2 (borders)` rows, identical to the original layout.
                let stream_chunks =
                    Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
                frame.render_widget(tabs_widget, stream_chunks[0]);

                let bottom_right_hint = if issue_badge.is_some() {
                    Some("[I]ssue detail")
                } else {
                    None
                };
                let mut state = crate::components::stream_pane::StreamViewState {
                    scroll_offset: self.scroll_offset,
                    is_following: self.is_following,
                };
                crate::components::stream_pane::render_stream(
                    frame,
                    stream_chunks[1],
                    &node.agent,
                    issue_badge,
                    bottom_right_hint,
                    stream_trace,
                    &mut state,
                );
                self.scroll_offset = state.scroll_offset;
                self.is_following = state.is_following;
            }
            _ => {
                self.render_non_stream(
                    frame,
                    area,
                    chunks[0],
                    body_area,
                    node,
                    issue_badge,
                    tabs_widget,
                );
            }
        }
    }

    fn build_tabs_widget(&self) -> Tabs<'static> {
        let titles: Vec<Line<'static>> = DetailTab::all()
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
        Tabs::new(titles)
            .select(
                DetailTab::all()
                    .iter()
                    .position(|t| *t == self.current_tab)
                    .unwrap_or(0),
            )
            .divider(" ")
    }

    fn render_non_stream(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        tabs_area: Rect,
        body_area: Rect,
        node: &ExecutorNode,
        issue_badge: Option<&str>,
        tabs_widget: Tabs<'static>,
    ) {
        let visible_h = body_area.height as usize;

        let body_lines = match self.current_tab {
            DetailTab::Stream => unreachable!("Stream handled in render()"),
            DetailTab::Artifacts => self.render_artifacts(node),
            DetailTab::Attempts => self.render_attempts(node),
            DetailTab::Task => self.render_task(node),
            DetailTab::Review => self.render_review(node),
        };
        let wrapped: Vec<Line<'static>> = body_lines
            .iter()
            .flat_map(|l| crate::components::line_wrap::wrap_line_to_width(l, body_area.width))
            .collect();
        let total = wrapped.len();

        // Apply the scroll clamp + re-engage-following BEFORE deriving the
        // label and rendering the block so the border reflects post-clamp
        // state on the same frame.
        let max_offset = total.saturating_sub(visible_h);
        if self.is_following {
            self.scroll_offset = max_offset;
        } else {
            self.scroll_offset = self.scroll_offset.min(max_offset);
            if self.scroll_offset >= max_offset && max_offset > 0 {
                self.is_following = true;
            }
        }

        let scroll_label_text = scroll_label(
            self.current_tab,
            total,
            visible_h,
            self.scroll_offset,
            self.is_following,
        );
        let pos_indicator = position_indicator(total, visible_h, self.scroll_offset, area.width);

        let mut block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .title(format!(" {} ", node.agent))
            .title_bottom(scroll_label_text.as_ref().to_string());
        if let Some(pos) = pos_indicator {
            block = block.title_bottom(
                Line::from(pos)
                    .alignment(Alignment::Right)
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        if let Some(badge) = issue_badge {
            block = block
                .title_top(Line::from(format!(" {} ", badge)).alignment(Alignment::Right))
                .title_bottom(Line::from(" [I]ssue detail ").alignment(Alignment::Right));
        }
        frame.render_widget(block, area);

        frame.render_widget(tabs_widget, tabs_area);

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
    fn stream_following_shows_following() {
        let s = scroll_label(DetailTab::Stream, 100, 20, 80, true);
        assert_eq!(s, Cow::Borrowed(" ▼ following "));
    }

    #[test]
    fn stream_not_following_shows_scroll_state() {
        let s = scroll_label(DetailTab::Stream, 100, 20, 42, false);
        assert_eq!(s, Cow::<'static, str>::Owned(" ▲ 42 ↑ ".to_string()));
    }

    #[test]
    fn stream_empty_total_shows_blank() {
        let s = scroll_label(DetailTab::Stream, 0, 20, 0, false);
        assert_eq!(s, Cow::Borrowed(""));
    }

    #[test]
    fn non_stream_empty_total_shows_blank() {
        let s = scroll_label(DetailTab::Artifacts, 0, 20, 0, false);
        assert_eq!(s, Cow::Borrowed(""));
    }

    #[test]
    fn non_stream_content_fits_viewport_shows_down() {
        // total=10, visible=20 → max_offset=0 → content fits.
        let s = scroll_label(DetailTab::Task, 10, 20, 0, false);
        assert_eq!(s, Cow::Borrowed(" ▼ "));
    }

    #[test]
    fn non_stream_at_end_following_shows_down() {
        // total=100, visible=20, offset=80, following → " ▼ ".
        let s = scroll_label(DetailTab::Attempts, 100, 20, 80, true);
        assert_eq!(s, Cow::Borrowed(" ▼ "));
    }

    #[test]
    fn non_stream_at_top_shows_top() {
        // total=100, visible=20, offset=0, not following → " top ".
        let s = scroll_label(DetailTab::Review, 100, 20, 0, false);
        assert_eq!(s, Cow::Borrowed(" top "));
    }

    #[test]
    fn non_stream_at_end_not_following_shows_end() {
        // total=100, visible=20, offset=80 (= max_offset), not following.
        let s = scroll_label(DetailTab::Artifacts, 100, 20, 80, false);
        assert_eq!(s, Cow::Borrowed(" end "));
    }

    #[test]
    fn non_stream_mid_scroll_shows_arrow_count() {
        // total=100, visible=20, offset=42 → " ▲ 42 ↑ ".
        let s = scroll_label(DetailTab::Artifacts, 100, 20, 42, false);
        assert_eq!(s, Cow::<'static, str>::Owned(" ▲ 42 ↑ ".to_string()));
    }
}

#[cfg(test)]
mod jump_to_tab_tests {
    use super::*;

    #[test]
    fn jump_to_review_resets_scroll_and_follow() {
        let mut pane = DetailPane::new();
        // Simulate user having scrolled on a prior tab.
        pane.scroll_offset = 42;
        pane.is_following = true;
        pane.jump_to_tab(DetailTab::Review);
        assert_eq!(pane.current_tab, DetailTab::Review);
        assert_eq!(pane.scroll_offset, 0);
        assert!(!pane.is_following);
    }

    #[test]
    fn jump_to_artifacts_opens_at_top_without_following() {
        let mut pane = DetailPane::new();
        pane.scroll_offset = 100;
        pane.is_following = true;
        pane.jump_to_tab(DetailTab::Artifacts);
        assert_eq!(pane.current_tab, DetailTab::Artifacts);
        assert_eq!(pane.scroll_offset, 0);
        assert!(!pane.is_following);
    }

    #[test]
    fn jump_to_stream_engages_follow_and_resets_offset() {
        let mut pane = DetailPane::new();
        // Start on a non-Stream tab with a non-zero scroll offset.
        pane.current_tab = DetailTab::Artifacts;
        pane.scroll_offset = 42;
        pane.is_following = false;
        pane.jump_to_tab(DetailTab::Stream);
        assert_eq!(pane.current_tab, DetailTab::Stream);
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.is_following);
    }

    #[test]
    fn jump_is_idempotent_on_same_tab() {
        // Jumping to the tab you are already on still resets.
        let mut pane = DetailPane::new();
        pane.current_tab = DetailTab::Task;
        pane.scroll_offset = 99;
        pane.is_following = true;
        pane.jump_to_tab(DetailTab::Task);
        assert_eq!(pane.current_tab, DetailTab::Task);
        assert_eq!(pane.scroll_offset, 0);
        assert!(!pane.is_following);
    }
}
