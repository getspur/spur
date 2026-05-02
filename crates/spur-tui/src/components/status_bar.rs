use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
#[cfg(feature = "analytics")]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::action::ViewId;
use crate::components::tombstone::{Tombstone, TombstoneKind};

pub struct StatusBar;

#[cfg(feature = "analytics")]
static VIA_ANALYTICS_VISIBLE: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "analytics")]
pub(crate) fn set_via_analytics_visible(visible: bool) {
    VIA_ANALYTICS_VISIBLE.store(visible, Ordering::Relaxed);
}

#[cfg(feature = "analytics")]
fn via_analytics_visible() -> bool {
    VIA_ANALYTICS_VISIBLE.load(Ordering::Relaxed)
}

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

pub fn render_tombstone_badge(slot: Option<&Tombstone>, now: std::time::Instant) -> Line<'static> {
    let Some(tombstone) = slot else {
        return Line::default();
    };

    let remaining = tombstone.expires_at.saturating_duration_since(now);
    let prefix = match &tombstone.kind {
        TombstoneKind::Reversible { .. } => "u:",
        TombstoneKind::QueuedRemote { .. } => "u: revert",
    };
    let label = if tombstone.label.chars().count() > 24 {
        let mut truncated: String = tombstone.label.chars().take(23).collect();
        truncated.push('…');
        truncated
    } else {
        tombstone.label.clone()
    };

    Line::from(vec![Span::styled(
        format!("  [{prefix} {label} {}s]", remaining.as_secs()),
        Style::default().fg(Color::DarkGray),
    )])
}

/// Returns the status-bar hint string for the SessionDetail view.
///
/// When `stream_in_flight` is true and the composer will not consume Esc,
/// the hint shows `[Esc]stop`; otherwise it shows `[Esc]back`. The caller is
/// responsible for AND-ing with `!cancelling_in_flight` before passing the
/// stream flag so the misleading `[Esc]stop` disappears once a cancel is
/// already in progress.
pub(crate) fn hint_for_session_detail(
    stream_in_flight: bool,
    esc_consumed_by_composer: bool,
) -> &'static str {
    if stream_in_flight && !esc_consumed_by_composer {
        " [Enter]send [Esc]stop [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-w]workers [Ctrl-r]history [?]help"
    } else {
        " [Enter]send [Esc]back [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-w]workers [Ctrl-r]history [?]help"
    }
}

/// Optional override for a view's StatusBar hint. Supports three policies:
/// - `full` is the canonical hint string.
/// - `compact` is an optional shorter alternative used when `full` doesn't fit.
/// - `hide_on_overflow` opt-in by views that have a separate footer carrying
///   the same hint (e.g., SessionPicker), so the StatusBar can render empty
///   when even `compact` doesn't fit, instead of truncating mid-word.
#[derive(Clone, Copy)]
pub struct HintOverride<'a> {
    pub full: &'a str,
    pub compact: Option<&'a str>,
    pub hide_on_overflow: bool,
}

impl<'a> HintOverride<'a> {
    /// Convenience constructor for views that have only a single hint
    /// and never want the StatusBar to hide on overflow.
    pub fn from_full(full: &'a str) -> Self {
        Self {
            full,
            compact: None,
            hide_on_overflow: false,
        }
    }
}

/// Everything the status bar needs to render one frame.
#[derive(Clone, Copy)]
pub struct StatusBarProps<'a> {
    pub view: &'a ViewId,
    pub tombstone: Option<&'a Tombstone>,
    pub running: usize,
    pub pending_review: usize,
    pub total_cost: f64,
    pub elapsed: &'a str,
    pub current_mode: Option<&'a str>,
    pub current_model_label: Option<&'a str>,
    pub current_effort_label: Option<&'a str>,
    pub usage_supported: bool,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
    /// True when the SessionDetail view has an in-flight stream; toggles
    /// the status-bar hint between `[Esc]back` (idle) and `[Esc]stop` (live).
    pub stream_in_flight: bool,
    /// True when the composer (InputBar) will consume the next Esc key
    /// (e.g. Vim Insert/Visual/Operator mode). Used to show `[Esc]back`
    /// instead of the misleading `[Esc]stop` when Esc cannot cancel.
    pub esc_consumed_by_composer: bool,
    /// Number of tracked issues (from IssuesLoaded); 0 means not shown.
    pub issue_count: usize,
    /// Graph alert summary from bv: (total, critical, warning). None if bv unavailable.
    pub alert_summary: Option<(usize, usize, usize)>,
    /// Compact license snapshot rendered as a pill, if licensing is active.
    pub license_badge: Option<&'a LicenseBadge>,
    /// Compact flag snapshot: (active_count, total_count). None if unavailable.
    pub flag_summary: Option<(usize, usize)>,
    /// When `Some`, overrides the hardcoded per-view hint string.
    /// Used by `SessionPickerView` to keep the StatusBar hint in sync
    /// with `footer_hint(...)` for the current picker mode.
    pub view_hint_override: Option<HintOverride<'a>>,
}

impl StatusBar {
    pub(crate) fn truncate_model_label(
        label: &str,
        available_width: u16,
    ) -> std::borrow::Cow<'_, str> {
        let available = usize::from(available_width);
        if available == 0 {
            return std::borrow::Cow::Borrowed("");
        }
        if label.chars().count() <= available {
            return std::borrow::Cow::Borrowed(label);
        }

        let mut shortened = label;
        for prefix in ["claude-3-5-", "claude-4-", "gpt-5-"] {
            if let Some(rest) = shortened.strip_prefix(prefix) {
                shortened = rest;
                break;
            }
        }

        if shortened.len() > 9 {
            let split = shortened.len() - 9;
            if shortened.is_char_boundary(split) {
                let (head, tail) = shortened.split_at(split);
                if tail.strip_prefix('-').is_some_and(|digits| {
                    digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit())
                }) {
                    shortened = head;
                }
            }
        }

        let cap = available.min(14);
        if shortened.chars().count() <= cap {
            return std::borrow::Cow::Borrowed(shortened);
        }
        if cap <= 1 {
            return std::borrow::Cow::Owned("…".to_string());
        }

        let mut out: String = shortened.chars().take(cap - 1).collect();
        out.push('…');
        std::borrow::Cow::Owned(out)
    }

    pub fn render(frame: &mut Frame, area: Rect, props: StatusBarProps<'_>) {
        let mode_text = props
            .current_mode
            .filter(|m| !m.is_empty())
            .map(|m| format!(" [{m}]"))
            .unwrap_or_default();

        let usage_text = match (
            props.usage_supported,
            props.context_used,
            props.context_size,
        ) {
            (false, _, _) => None,
            (true, Some(used), Some(size)) if size > 0 => {
                let pct = (used as f64 / size as f64) * 100.0;
                Some(format!("ctx {:.0}%", pct))
            }
            (true, _, _) => Some("ctx --%".to_string()),
        };

        // Build the review span: yellow+bold when reviews are pending, dark-gray otherwise.
        let review_style = if props.pending_review > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        // Try full metrics first; fall back to compact if they crowd out hints.
        let full_spans = Self::metric_spans(
            &props,
            mode_text.clone(),
            usage_text.clone(),
            review_style,
            false,
        );
        let full_line = Line::from(full_spans);
        let full_width = full_line.width() as u16;

        let compact_spans = Self::metric_spans(&props, mode_text, usage_text, review_style, true);
        let compact_line = Line::from(compact_spans);
        let compact_width = compact_line.width() as u16;

        // SessionDetail has a dedicated hint area allocated separately below, so the
        // 45-col reserve doesn't apply — fit metrics fully unless they exceed area.
        // Other views render hints inline, so reserve space to keep them readable.
        let use_compact = if matches!(props.view, ViewId::SessionDetail(_)) {
            full_width > area.width
        } else {
            let hints_reserve = 45u16;
            full_width + hints_reserve > area.width && compact_width + hints_reserve <= area.width
        };
        let (right, right_width) = if use_compact {
            (compact_line, compact_width)
        } else {
            (full_line, full_width)
        };
        let right_width = right_width.min(area.width);

        let hint_area_width = area
            .width
            .saturating_sub(right_width.max(1))
            .saturating_sub(2);
        let resolved_hint: &str = if let Some(o) = props.view_hint_override.as_ref() {
            let full_w = Span::raw(o.full).width() as u16;
            let compact_w = o.compact.map(|c| Span::raw(c).width() as u16);

            if full_w <= hint_area_width {
                o.full
            } else if matches!(compact_w, Some(w) if w <= hint_area_width) {
                o.compact.unwrap()
            } else if o.hide_on_overflow {
                ""
            } else if let Some(c) = o.compact {
                c
            } else {
                o.full
            }
        } else {
            match props.view {
                ViewId::Dashboard => {
                    " [i]nput [Enter]focus [r]eview [s]essions [Esc]back [Ctrl+C]quit [?]help"
                }
                ViewId::IssueBrowser => {
                    " [j/k]navigate [Enter]detail [o/w/b/x]status [W]work [Esc]back [?]help"
                }
                ViewId::PlanBrowser => {
                    " [j/k]navigate [Enter]open [R]resume [e]epic [r]refresh [b]backlog [Esc]back"
                }
                ViewId::SessionDetail(_) => {
                    hint_for_session_detail(props.stream_in_flight, props.esc_consumed_by_composer)
                }
                ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
                ViewId::PlanInspector(_) => " [Esc]back [Alt-p]close",
                ViewId::Insights => "Insights",
                #[cfg(feature = "markdown")]
                ViewId::MermaidOverlay(_) => " [Esc]close",
            }
        };
        let hints = resolved_hint;

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

    /// Build the right-hand metric spans. When `compact` is true, use
    /// abbreviated symbols and drop low-priority items so the status bar
    /// stays readable on narrow terminals.
    fn metric_spans<'a>(
        props: &StatusBarProps<'a>,
        mode_text: String,
        usage_text: Option<String>,
        review_style: Style,
        compact: bool,
    ) -> Vec<Span<'a>> {
        let sep = if compact { " " } else { " · " };
        let mut spans: Vec<Span<'a>> = Vec::new();

        if props.issue_count > 0 {
            spans.push(Span::styled(
                if compact {
                    format!("{}i", props.issue_count)
                } else {
                    format!("{} issues", props.issue_count)
                },
                Style::default().fg(Color::Cyan),
            ));
            spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        }
        if let Some((total, critical, _warning)) = props.alert_summary {
            if total > 0 {
                let style = if critical > 0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                spans.push(Span::styled(
                    if compact {
                        format!("!{total}")
                    } else {
                        format!("{total} alerts")
                    },
                    style,
                ));
                spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
            }
        }
        if let Some(badge) = props.license_badge {
            spans.push(Span::styled(format!("{} ", badge.label), badge.style()));
            if !compact {
                spans.push(Span::styled("· ", Style::default().fg(Color::DarkGray)));
            }
        }
        let tombstone_badge = render_tombstone_badge(props.tombstone, std::time::Instant::now());
        if !tombstone_badge.spans.is_empty() {
            spans.extend(tombstone_badge.spans);
            spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        }
        #[cfg(feature = "analytics")]
        if via_analytics_visible() {
            spans.push(Span::styled(
                "via analytics",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        }
        if let Some((active, total)) = props.flag_summary {
            let flag_style = if active == total {
                Style::default().fg(Color::Green)
            } else if active == 0 {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            spans.push(Span::styled(
                if compact {
                    format!("F:{active}/{total}")
                } else {
                    format!("flags {active}/{total}")
                },
                flag_style,
            ));
            spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        }

        spans.push(Span::styled(
            if compact {
                format!("▶{}", props.running)
            } else {
                format!("{} running", props.running)
            },
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            if compact {
                format!("R{}", props.pending_review)
            } else {
                format!("{} review", props.pending_review)
            },
            review_style,
        ));
        spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("${:.2}", props.total_cost),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("{} ", props.elapsed),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(mode_text, Style::default().fg(Color::Magenta)));

        let mut has_status_segment = false;
        if let Some(model) = props.current_model_label.filter(|label| !label.is_empty()) {
            let model = Self::truncate_model_label(model, if compact { 14 } else { 24 });
            spans.push(Span::styled(
                format!(" {}", model),
                Style::default().fg(Color::White),
            ));
            has_status_segment = true;
        }
        if !compact {
            if let Some(effort) = props.current_effort_label.filter(|label| !label.is_empty()) {
                if has_status_segment {
                    spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
                } else {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    effort,
                    Style::default().fg(Color::LightMagenta),
                ));
                has_status_segment = true;
            }
        }
        if let Some(usage_text) = usage_text {
            if has_status_segment {
                spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                usage_text,
                Style::default().fg(Color::LightBlue),
            ));
        }

        if !compact {
            spans.push(Span::styled(
                "[Ctrl+K: go] ",
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(
                "?: help",
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            "spur",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans
    }
}

#[cfg(test)]
mod status_bar_hint_tests {
    use super::{hint_for_session_detail, StatusBar};

    #[test]
    fn hint_shows_stop_when_streaming_and_esc_can_cancel() {
        let hint = hint_for_session_detail(true, false);
        assert!(hint.contains("[Esc]stop"), "got: {hint}");
        assert!(!hint.contains("[Esc]back"));
    }

    #[test]
    fn hint_shows_back_when_streaming_but_esc_is_consumed_by_vim_mode() {
        let hint = hint_for_session_detail(true, true);
        assert!(hint.contains("[Esc]back"), "got: {hint}");
        assert!(!hint.contains("[Esc]stop"));
    }

    #[test]
    fn hint_shows_back_when_idle() {
        let hint = hint_for_session_detail(false, false);
        assert!(hint.contains("[Esc]back"), "got: {hint}");
        assert!(!hint.contains("[Esc]stop"));
    }

    #[test]
    fn truncate_model_label_strips_common_prefix_and_caps_length() {
        let label = StatusBar::truncate_model_label("gpt-5-super-long-model-name", 14);
        assert_eq!(label.as_ref(), "super-long-mo…");
    }

    #[test]
    fn truncate_model_label_strips_date_suffix_after_prefix() {
        let label = StatusBar::truncate_model_label("claude-3-5-sonnet-20241022", 14);
        assert_eq!(label.as_ref(), "sonnet");
    }

    #[test]
    fn truncate_model_label_handles_multibyte_text_near_date_suffix() {
        let date_suffixed = StatusBar::truncate_model_label("a名前-20241022", 4);
        assert_eq!(date_suffixed.as_ref(), "a名前");

        let truncated = StatusBar::truncate_model_label("a-名前20241022", 4);
        assert_eq!(truncated.as_ref(), "a-名…");
    }
}
