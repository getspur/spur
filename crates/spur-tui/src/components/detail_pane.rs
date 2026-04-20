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

    /// Private shared helper. Encodes the per-tab reset invariants so
    /// every entry point (`cycle_tab`, `jump_to_tab`) cannot accidentally
    /// diverge. Opens at top (`scroll_offset = 0`) on every tab; sets
    /// `is_following` based on the destination tab kind; snaps the
    /// Stream trace to bottom when landing on Stream.
    fn set_tab(
        &mut self,
        tab: DetailTab,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        self.current_tab = tab;
        self.scroll_offset = 0;
        match tab {
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

    /// Cycle to the next (or previous) tab.
    ///
    /// Per-tab reset invariants (`scroll_offset = 0`, `is_following`
    /// per tab kind) are centralised in [`DetailPane::set_tab`].
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
        self.set_tab(all[next], stream_trace);
    }

    /// Jump directly to a specific tab. Applies the same per-tab reset
    /// invariants as [`DetailPane::cycle_tab`] (scroll to top, set
    /// `is_following` per tab kind, snap Stream trace to bottom).
    ///
    /// Use this from outside the pane instead of writing `current_tab`
    /// directly — the field is `pub(crate)` and only readable via
    /// [`DetailPane::current_tab`].
    pub fn jump_to_tab(
        &mut self,
        tab: DetailTab,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        self.set_tab(tab, stream_trace);
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
        // ── 1. Compute the trace-follow flag (Stream tab authoritative
        //       source) before any rendering side-effects. ─────────────
        let trace_following: Option<bool> = match self.current_tab {
            DetailTab::Stream => stream_trace.as_deref().map(|t| t.is_following()),
            _ => None,
        };

        // ── 2. Skeleton block — shape-equivalent to the final block so
        //       Block::inner() returns the same rect. ──────────────────
        //
        // Every title POSITION that will appear on the final block must
        // also appear on the skeleton. Content can be placeholder because
        // inner() is a function of borders + title presence, not content.
        let mut skeleton = Block::default()
            .borders(Borders::ALL)
            .title(" ")              // matches final top-left (agent name)
            .title_bottom(" ");      // matches final bottom-left (scroll_label)
        if issue_badge.is_some() {
            skeleton = skeleton
                .title_top(Line::from(" ").alignment(Alignment::Right))   // matches final top-right (badge)
                .title_bottom(Line::from(" ").alignment(Alignment::Right)); // matches final bottom-right ([I]ssue)
        }
        let inner = skeleton.inner(area);
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        let body_area = chunks[1];

        // ── 3. Compute body content + metrics for non-Stream (and Stream
        //       placeholder) paths. For Stream-with-trace, body is owned
        //       by ReactTrace::render_compact; total/visible still
        //       meaningful only for the `scroll_label` derivation. ─────
        let stream_with_trace = matches!(self.current_tab, DetailTab::Stream)
            && stream_trace.is_some();

        // `wrapped` is only populated for paths that render a Paragraph.
        let mut wrapped: Vec<Line<'static>> = Vec::new();
        let visible_h = body_area.height as usize;
        let total: usize;

        if stream_with_trace {
            // ReactTrace owns the body; we do not wrap. `total` is not used
            // for the Stream label (trace_following is authoritative).
            total = 0;
        } else {
            let body_lines = match self.current_tab {
                DetailTab::Stream => {
                    // No trace materialized yet (orphan event or first-load race).
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
            wrapped = body_lines
                .iter()
                .flat_map(|l| crate::components::line_wrap::wrap_line_to_width(l, body_area.width))
                .collect();
            total = wrapped.len();
        }

        // ── 4. Apply the scroll clamp + re-engage-following BEFORE
        //       deriving the label and rendering the block. This fixes
        //       the one-frame lag where the border used to show stale
        //       "not following" on the frame the user reached bottom. ─
        if !stream_with_trace {
            let max_offset = total.saturating_sub(visible_h);
            if self.is_following {
                self.scroll_offset = max_offset;
            } else {
                self.scroll_offset = self.scroll_offset.min(max_offset);
                if self.scroll_offset >= max_offset && max_offset > 0 {
                    self.is_following = true;
                }
            }
        }

        // ── 5. Derive the scroll label from final post-clamp state. ──
        let scroll_label_text = scroll_label(
            self.current_tab,
            total,
            visible_h,
            self.scroll_offset,
            self.is_following,
            trace_following,
        );

        // ── 6. Build the real block with all titles. ─────────────────
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", node.agent))
            .title_bottom(scroll_label_text.as_ref().to_string());
        if let Some(badge) = issue_badge {
            block = block
                .title_top(Line::from(format!(" {} ", badge)).alignment(Alignment::Right))
                .title_bottom(Line::from(" [I]ssue detail ").alignment(Alignment::Right));
        }
        frame.render_widget(block, area);

        // ── 7. Render tabs. ──────────────────────────────────────────
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

        // ── 8. Render body. ──────────────────────────────────────────
        if stream_with_trace {
            let trace = stream_trace.expect("stream_with_trace implies Some");
            trace.render_compact(frame, body_area);
            return;
        }
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

#[cfg(test)]
mod jump_to_tab_tests {
    use super::*;

    #[test]
    fn jump_to_review_resets_scroll_and_follow() {
        let mut pane = DetailPane::new();
        // Simulate user having scrolled on a prior tab.
        pane.scroll_offset = 42;
        pane.is_following = true;
        pane.jump_to_tab(DetailTab::Review, None);
        assert_eq!(pane.current_tab, DetailTab::Review);
        assert_eq!(pane.scroll_offset, 0);
        assert!(!pane.is_following);
    }

    #[test]
    fn jump_to_artifacts_opens_at_top_without_following() {
        let mut pane = DetailPane::new();
        pane.scroll_offset = 100;
        pane.is_following = true;
        pane.jump_to_tab(DetailTab::Artifacts, None);
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
        pane.jump_to_tab(DetailTab::Stream, None);
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
        pane.jump_to_tab(DetailTab::Task, None);
        assert_eq!(pane.current_tab, DetailTab::Task);
        assert_eq!(pane.scroll_offset, 0);
        assert!(!pane.is_following);
    }
}
