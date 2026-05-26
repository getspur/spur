use std::time::Instant;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::action::ViewId;
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::theme::Theme;

use super::{token, FocusedSessionPanel, LoadState, SessionDetailView, CANCEL_HINT_TEXT};

pub(super) fn build_auth_banner_widget<'a>(message: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(message)
        .style(
            Style::default()
                .fg(token(theme, "session_detail.auth_banner.fg"))
                .bg(token(theme, "session_detail.auth_banner.bg"))
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title("Authentication required"),
        )
}

pub(super) fn build_session_error_widget<'a>(message: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(message)
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(token(theme, "session_detail.error_banner.fg"))
                .bg(token(theme, "session_detail.error_banner.bg"))
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title("Session error"),
        )
}

/// Render the centered "Cancel turn? [y]es / [n]o" confirmation modal as
/// an overlay over the trace pane. Drawn last in `render_inner` so it sits
/// on top of everything else.
///
/// Always renders SOMETHING visible while `cancel_confirm_open == true` —
/// silent no-render would create an invisible focus trap (the modal-open
/// key handler swallows all keys; if the user can't see why, they can't
/// recover). On terminals smaller than the preferred 50×5 the modal
/// degrades to a compact single-line prompt clamped to the available
/// width.
pub(super) fn render_cancel_confirm_modal(frame: &mut Frame, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    const PREFERRED_WIDTH: u16 = 50;
    const PREFERRED_HEIGHT: u16 = 5;
    const MINIMAL_PROMPT: &str = "Cancel turn? [y]es / [n]o";

    let modal_width = PREFERRED_WIDTH.min(area.width);
    let modal_height = PREFERRED_HEIGHT.min(area.height);
    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let y = area.y + (area.height.saturating_sub(modal_height)) / 2;
    let rect = Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    let style = Style::default()
        .fg(token(theme, "session_detail.cancel_modal.fg"))
        .bg(token(theme, "session_detail.cancel_modal.bg"))
        .add_modifier(Modifier::BOLD);

    // Bordered block needs ≥3 cols × ≥3 rows for top/bottom borders +
    // content. Below that, fall back to a borderless single-line prompt
    // clamped to the available width so the user is never left in an
    // invisible-modal focus trap.
    let widget = if modal_width >= 3 && modal_height >= 3 {
        Paragraph::new("Cancel turn?\n\n[y]es / [n]o")
            .alignment(Alignment::Center)
            .style(style)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Confirm cancel "),
            )
    } else {
        let truncated: String = MINIMAL_PROMPT.chars().take(modal_width as usize).collect();
        Paragraph::new(truncated)
            .alignment(Alignment::Left)
            .style(style)
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(widget, rect);
}

impl SessionDetailView {
    pub(super) fn render_inner(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &super::super::ViewContext,
    ) {
        let lineage = Some(ctx.lineage);
        let tracked_plan = ctx.plan_projection.current_for_session(self.session_id());
        let license_badge = ctx.license_badge;
        let flag_summary = ctx.flag_summary;
        let view_hint_override = if self
            .cancel_hint_until
            .is_some_and(|until| until > Instant::now())
        {
            Some(HintOverride::from_full(CANCEL_HINT_TEXT))
        } else {
            ctx.transient_hint_override
        };

        // Pre-ready render path: show a status label until LoadState::Ready.
        match &self.load_state {
            LoadState::Retiring => {
                render_load_label(frame, area, "Retiring previous session…");
                return;
            }
            LoadState::Connecting { brain_name } => {
                let label = if brain_name.is_empty() {
                    "Connecting to brain…".to_string()
                } else {
                    format!("Connecting to {brain_name}…")
                };
                render_load_label(frame, area, &label);
                return;
            }
            LoadState::Loading => {
                render_load_label(frame, area, "Loading session history…");
                return;
            }
            LoadState::Failed { message } => {
                render_error_label(frame, area, message, ctx.theme);
                return;
            }
            LoadState::Ready => {
                // Fall through to the full render path below.
            }
        }

        let elapsed = self.elapsed();

        // Reserve the top row for the (non-blocking) resume banner when
        // visible. Subsequent banner/content layout operates on `area_rest`.
        let (resume_banner_area, area_rest) = if self.banner_is_visible() {
            let banner = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 1,
            };
            let rest = Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width,
                height: area.height.saturating_sub(1),
            };
            (Some(banner), rest)
        } else {
            (None, area)
        };

        // If an auth error is active, split off the top 3 rows for a red
        // banner. This preserves the rest of the layout exactly as before.
        let (banner_area, content_area) = if self.auth_error.is_some() {
            let [banner, content] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area_rest);
            (Some(banner), content)
        } else {
            (None, area_rest)
        };

        if let (Some(banner_area), Some(msg)) = (banner_area, self.auth_error.as_ref()) {
            let banner = build_auth_banner_widget(msg.as_str(), ctx.theme);
            frame.render_widget(banner, banner_area);
        }

        let input_height = self.input_bar.required_height(content_area.width);
        let graph_hint = (!self.fs_unsafe)
            .then(|| self.mention_registry.borrow().code_graph_hint())
            .flatten();
        let pre_input_banner_height = u16::from(self.fs_unsafe || graph_hint.is_some());

        // Compute workers panel height (dynamic: 0 when no active workers).
        // Suppress on very small terminals to avoid squeezing the trace.
        let executor_ids = self.react_trace.active_executor_ids();
        let workers_h = if content_area.height < 12 {
            0
        } else {
            lineage
                .map(|lin| {
                    crate::components::workers_panel::compute_height(
                        lin,
                        &executor_ids,
                        self.workers_panel_collapsed,
                    )
                })
                .unwrap_or(0)
        };

        let chunks = Layout::vertical([
            Constraint::Length(1),                       // header
            Constraint::Min(4),                          // react trace (fills)
            Constraint::Length(workers_h),               // workers panel
            Constraint::Length(pre_input_banner_height), // pre-input banner
            Constraint::Length(input_height),            // input bar
            Constraint::Length(1),                       // status bar
        ])
        .split(content_area);

        // ── Header: breadcrumb + elapsed + cost ─────────────────────────
        let [header_left, header_right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(48)]).areas(chunks[0]);
        let header = Line::from(vec![
            Span::styled(
                " Dashboard > ",
                Style::default().fg(token(ctx.theme, "session_detail.breadcrumb.fg")),
            ),
            Span::styled(
                &self.agent_name,
                Style::default()
                    .fg(token(ctx.theme, "session_detail.agent_name.fg"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", self.role),
                Style::default().fg(token(ctx.theme, "session_detail.role.fg")),
            ),
            Span::raw("  "),
            Span::styled(
                &elapsed,
                Style::default().fg(token(ctx.theme, "session_detail.elapsed.fg")),
            ),
            Span::raw("  "),
            Span::styled(
                format!("${:.2}", self.cost),
                Style::default().fg(token(ctx.theme, "session_detail.cost.fg")),
            ),
            if self.fs_unsafe {
                Span::styled(
                    "  unsafe-fs",
                    Style::default()
                        .fg(token(ctx.theme, "session_detail.unsafe_fs.fg"))
                        .bg(token(ctx.theme, "session_detail.unsafe_fs.bg"))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("")
            },
        ]);
        frame.render_widget(Paragraph::new(header), header_left);
        if let Some(plan) = tracked_plan {
            crate::components::plan_pulse::render(frame, header_right, plan);
        }

        // ── React trace ─────────────────────────────────────────────────
        // Push the active theme onto the trace before render so token
        // resolution honors the user's theme choice (ReactTrace caches
        // theme as component state; see set_theme docs in mod.rs).
        self.react_trace.set_theme(ctx.theme);
        #[cfg(feature = "markdown")]
        {
            let mut rt_ctx = crate::components::react_trace::RenderContext {
                mermaid_registry: &self.mermaid_registry,
                mermaid_registry_version: self.mermaid_registry_version,
                picker: self.render_picker.as_ref(),
                image_cache: &mut self.image_cache,
            };
            self.react_trace.render_with_ctx_focused(
                frame,
                chunks[1],
                &mut rt_ctx,
                lineage,
                self.focused_panel == FocusedSessionPanel::ReactTrace,
            );
        }
        #[cfg(not(feature = "markdown"))]
        self.react_trace.render_focused(
            frame,
            chunks[1],
            lineage,
            self.focused_panel == FocusedSessionPanel::ReactTrace,
        );

        // After react_trace render — re-raster on bucket-up.
        #[cfg(feature = "markdown")]
        {
            let cell_w_px = self
                .render_picker
                .as_ref()
                .map(|p| p.font_size().0)
                .unwrap_or(8);
            self.maybe_request_rerasters(chunks[1].width, cell_w_px);
        }

        // ── Workers panel ───────────────────────────────────────────────
        if let Some(lin) = lineage {
            if workers_h > 0 {
                crate::components::workers_panel::render_focused(
                    frame,
                    chunks[2],
                    lin,
                    &executor_ids,
                    self.workers_panel_collapsed,
                    self.focused_panel == FocusedSessionPanel::Workers,
                );
            }
        }

        // ── Input bar ───────────────────────────────────────────────────
        if self.fs_unsafe {
            let banner = Line::from(Span::styled(
                " unsafe-fs: flock unsupported on this volume - multi-window protection OFF ",
                Style::default()
                    .fg(token(ctx.theme, "session_detail.unsafe_fs.fg"))
                    .bg(token(ctx.theme, "session_detail.unsafe_fs.bg")),
            ));
            frame.render_widget(Paragraph::new(banner), chunks[3]);
        } else if let Some(hint) = graph_hint {
            let banner = Line::from(Span::styled(
                format!(" {hint} "),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(banner), chunks[3]);
        }

        // Render in "inert" style (dimmed border, no terminal cursor) when
        // a PickerShell has the focus — the shell owns the cursor.
        if self.completion.is_active() {
            self.input_bar.render_inert(frame, chunks[4]);
        } else {
            self.input_bar.render(frame, chunks[4]);
        }

        // ── PickerShell overlay ─────────────────────────────────────────
        self.completion.render(frame, chunks[4], area, ctx.theme);

        // ── Status bar (with live worker counts) ────────────────────────
        let (running, pending_review) = lineage
            .map(|lin| {
                let mut r = 0usize;
                let mut p = 0usize;
                for eid in &executor_ids {
                    if let Some(node) = lin.node(&spur_core::ExecutorId(eid.clone())) {
                        match node.phase {
                            spur_acp::domain::events::LifecycleState::Running
                            | spur_acp::domain::events::LifecycleState::Spawning
                            | spur_acp::domain::events::LifecycleState::Resuming => r += 1,
                            spur_acp::domain::events::LifecycleState::AwaitingReview => p += 1,
                            _ => {}
                        }
                    }
                }
                (r, p)
            })
            .unwrap_or((0, 0));
        let caps = self.spur_agent_caps.as_deref();
        let model_label = self.resolved_model_label();
        let effort_label = spur_acp::SpurAgentCaps::effort_label_from(&self.session_config_options);
        let usage_supported = caps
            .map(spur_acp::SpurAgentCaps::usage_supported)
            .unwrap_or(true);

        StatusBar::render(
            frame,
            chunks[5],
            StatusBarProps {
                view: &ViewId::SessionDetail(self.session_id.clone()),
                theme: ctx.theme,
                tombstone: ctx.tombstone,
                running,
                pending_review,
                total_cost: self.cost,
                elapsed: &elapsed,
                current_mode: self.current_mode.as_deref(),
                current_model_label: model_label.as_deref(),
                current_effort_label: effort_label.as_deref(),
                usage_supported,
                context_used: self.context_used,
                context_size: self.context_size,
                stream_in_flight: self.stream_in_flight && !self.cancelling_in_flight,
                esc_consumed_by_composer: self.input_bar.wants_esc(),
                issue_count: 0,
                alert_summary: None,
                license_badge,
                flag_summary,
                view_hint_override,
            },
        );

        // ── Resume banner (top row, if visible) ─────────────────────────
        if let (Some(banner), Some(rect)) = (self.resume_banner.as_ref(), resume_banner_area) {
            banner.render(frame, rect);
            if self.ready_banner.is_some() {
                tracing::warn!(
                    "ready_banner and resume_banner both set — auto-resume wins (spec R3 violation)"
                );
            }
        } else if let (Some(ready_text), Some(rect)) =
            (self.ready_banner.as_ref(), resume_banner_area)
        {
            let styled = Paragraph::new(ready_text.as_str())
                .style(Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC));
            frame.render_widget(styled, rect);
        }

        // ── Cancel-confirm modal (drawn last so it sits on top) ─────────
        if self.cancel_confirm_open {
            render_cancel_confirm_modal(frame, area, ctx.theme);
        }
    }
}

// ─── LoadState render helpers ───────────────────────────────────────────────

/// Render a centered single-line status label for pre-ready LoadStates
/// (`Retiring`, `Connecting`, `Loading`).
pub(super) fn render_load_label(frame: &mut Frame, area: Rect, label: &str) {
    let para = Paragraph::new(label)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));
    // Centre vertically by splitting the area in thirds.
    let [_, mid, _] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Min(1),
        Constraint::Percentage(60),
    ])
    .areas(area);
    frame.render_widget(para, mid);
}

/// Render a red error panel for `LoadState::Failed`.
pub(super) fn render_error_label(frame: &mut Frame, area: Rect, message: &str, theme: &Theme) {
    let para = build_session_error_widget(message, theme);
    let [_, mid, _] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Min(3),
        Constraint::Percentage(60),
    ])
    .areas(area);
    frame.render_widget(para, mid);
}

// ─── Formatting helpers (test-only; production path uses dispatch.rs) ───

#[cfg(test)]
/// Extract renderable text from a `ToolCallContent` slice.
///
/// Handles all known variants:
/// - `Content` — returns the inner text (non-text blocks silently skipped).
/// - `Diff`     — formats as a truncated unified-style diff (max `DIFF_MAX_LINES` body lines).
/// - `Terminal` — returns a placeholder `[terminal: <id>]`.
/// - Unknown future variants — silently ignored (`ToolCallContent` is `#[non_exhaustive]`).
///
/// Returns `None` if nothing renderable was produced.
pub(super) fn extract_tool_call_text(content: &[spur_acp::ToolCallContent]) -> Option<String> {
    use spur_acp::ToolCallContent;
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content(cb) => {
                if let spur_acp::ContentBlock::Text(tc) = &cb.content {
                    out.push_str(&tc.text);
                }
                // Non-Text ContentBlock variants (Image, Audio, Resource) silently skipped.
            }
            ToolCallContent::Diff(diff) => {
                out.push_str(&format_diff_truncated(
                    &diff.path.display().to_string(),
                    diff.old_text.as_deref(),
                    &diff.new_text,
                ));
            }
            ToolCallContent::Terminal(term) => {
                // TerminalId derives Display; fall back to .0 (Arc<str>) if needed.
                out.push_str(&format!("[terminal: {}]", term.terminal_id));
            }
            _ => {
                // ToolCallContent is #[non_exhaustive]; ignore unknown variants.
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
const DIFF_MAX_LINES: usize = 40;

/// Format a diff as a simplified unified-diff string, capped at `DIFF_MAX_LINES` body lines.
///
/// Old lines are prefixed with `-`, new lines with `+`. This is NOT an LCS diff;
/// it renders the old text as all-deletions and the new text as all-additions,
/// matching how `ObservePayload::EditResult.diff` is rendered elsewhere in the TUI.
#[cfg(test)]
pub(super) fn format_diff_truncated(path: &str, old: Option<&str>, new_: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path));
    out.push_str(&format!("+++ b/{}\n", path));

    let mut body_lines: usize = 0;
    let mut truncated_count: usize = 0;

    if let Some(old_text) = old {
        for line in old_text.lines() {
            if body_lines >= DIFF_MAX_LINES {
                truncated_count += 1;
                continue;
            }
            out.push_str(&format!("-{}\n", line));
            body_lines += 1;
        }
    }
    for line in new_.lines() {
        if body_lines >= DIFF_MAX_LINES {
            truncated_count += 1;
            continue;
        }
        out.push_str(&format!("+{}\n", line));
        body_lines += 1;
    }
    if truncated_count > 0 {
        out.push_str(&format!("... ({} more lines)\n", truncated_count));
    }
    out
}
