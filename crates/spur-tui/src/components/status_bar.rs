use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::action::ViewId;

pub struct StatusBar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseBadge {
    pub label: String,
    pub tone: LicenseBadgeTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseBadgeTone {
    Neutral,
    Success,
    Warning,
    Danger,
}

impl LicenseBadge {
    pub fn new(label: impl Into<String>, tone: LicenseBadgeTone) -> Self {
        Self {
            label: label.into(),
            tone,
        }
    }

    fn style(&self) -> Style {
        match self.tone {
            LicenseBadgeTone::Neutral => Style::default().fg(Color::DarkGray),
            LicenseBadgeTone::Success => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            LicenseBadgeTone::Warning => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            LicenseBadgeTone::Danger => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
        }
    }
}

/// Returns the status-bar hint string for the SessionDetail view.
///
/// When `stream_in_flight` is true the hint shows `[Esc]stop`; when the
/// stream is idle it shows `[Esc]back`.  The caller is responsible for
/// AND-ing with `!cancelling_in_flight` before passing the flag so the
/// misleading `[Esc]stop` disappears once a cancel is already in progress.
pub(crate) fn hint_for_session_detail(stream_in_flight: bool) -> &'static str {
    if stream_in_flight {
        " [Enter]send [Esc]stop [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-w]workers [Ctrl-r]history [?]help"
    } else {
        " [Enter]send [Esc]back [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-w]workers [Ctrl-r]history [?]help"
    }
}

/// Everything the status bar needs to render one frame.
#[derive(Clone, Copy)]
pub struct StatusBarProps<'a> {
    pub view: &'a ViewId,
    pub running: usize,
    pub pending_review: usize,
    pub total_cost: f64,
    pub elapsed: &'a str,
    pub current_mode: Option<&'a str>,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
    /// True when the SessionDetail view has an in-flight stream; toggles
    /// the status-bar hint between `[Esc]back` (idle) and `[Esc]stop` (live).
    pub stream_in_flight: bool,
    /// Number of tracked issues (from IssuesLoaded); 0 means not shown.
    pub issue_count: usize,
    /// Graph alert summary from bv: (total, critical, warning). None if bv unavailable.
    pub alert_summary: Option<(usize, usize, usize)>,
    /// Compact license snapshot rendered as a pill, if licensing is active.
    pub license_badge: Option<&'a LicenseBadge>,
    /// Compact flag snapshot: (active_count, total_count). None if unavailable.
    pub flag_summary: Option<(usize, usize)>,
}

impl StatusBar {
    pub fn render(frame: &mut Frame, area: Rect, props: StatusBarProps<'_>) {
        let hints = match props.view {
            ViewId::Dashboard => {
                " [i]nput [Enter]focus [r]eview [s]essions [Esc]back [Ctrl+C]quit [?]help"
            }
            ViewId::SessionDetail(_) => hint_for_session_detail(props.stream_in_flight),
            ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => " [Esc]close",
        };

        let mode_text = props
            .current_mode
            .filter(|m| !m.is_empty())
            .map(|m| format!(" [{m}]"))
            .unwrap_or_default();

        let usage_text = match (props.context_used, props.context_size) {
            (Some(used), Some(size)) if size > 0 => {
                let pct = (used as f64 / size as f64) * 100.0;
                format!(" ctx {:.0}%", pct)
            }
            _ => String::new(),
        };

        // Build the review span: yellow+bold when reviews are pending, dark-gray otherwise.
        let review_style = if props.pending_review > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut right_spans: Vec<Span> = Vec::new();
        if props.issue_count > 0 {
            right_spans.push(Span::styled(
                format!("{} issues", props.issue_count),
                Style::default().fg(Color::Cyan),
            ));
            right_spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        if let Some((total, critical, _warning)) = props.alert_summary {
            if total > 0 {
                let style = if critical > 0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                right_spans.push(Span::styled(format!("{total} alerts"), style));
                right_spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            }
        }
        if let Some(badge) = props.license_badge {
            right_spans.push(Span::styled(format!("{} ", badge.label), badge.style()));
            right_spans.push(Span::styled("· ", Style::default().fg(Color::DarkGray)));
        }
        if let Some((active, total)) = props.flag_summary {
            let flag_style = if active == total {
                Style::default().fg(Color::Green)
            } else if active == 0 {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            right_spans.push(Span::styled(format!("F:{active}/{total}"), flag_style));
            right_spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        right_spans.extend([
            Span::styled(
                format!("{} running", props.running),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(" · ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} review", props.pending_review), review_style),
            Span::styled(" · ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${:.2}", props.total_cost),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(" · {} ", props.elapsed),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(mode_text, Style::default().fg(Color::Magenta)),
            Span::styled(usage_text, Style::default().fg(Color::LightBlue)),
            Span::styled("[Ctrl+K: go] ", Style::default().fg(Color::DarkGray)),
            Span::styled("?: help", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(
                "spur",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        let right = Line::from(right_spans);
        let right_width = right.width() as u16;
        let hints_line = Line::from(Span::styled(
            hints,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::DIM),
        ));

        // Right-align the metric/brand group; let the hints take the rest.
        let [hints_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width.max(1))])
                .areas(area);

        frame.render_widget(Paragraph::new(hints_line), hints_area);
        frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
    }
}

#[cfg(test)]
mod status_bar_hint_tests {
    use super::hint_for_session_detail;

    #[test]
    fn hint_shows_stop_when_stream_in_flight() {
        let hint = hint_for_session_detail(true);
        assert!(hint.contains("[Esc]stop"), "got: {hint}");
        assert!(!hint.contains("[Esc]back"));
    }

    #[test]
    fn hint_shows_back_when_idle() {
        let hint = hint_for_session_detail(false);
        assert!(hint.contains("[Esc]back"), "got: {hint}");
        assert!(!hint.contains("[Esc]stop"));
    }
}
