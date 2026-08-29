use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::components::trace_format::{
    derive_delegate_status, family_glyph, input_display_lines, input_summary, observe_compact,
    observe_payload_lines, observe_verb, outcome_glyph, terminal_safe_text,
};
use crate::theme::{resolve_token, ColorDepth};

use super::types::{ActStatus, TraceKind};
use super::ReactTrace;
use crate::components::spinner;

/// Pad a wrapped line out to `width` cells with a single trailing
/// background-coloured space span, but only when the line is part of a
/// "bubble" — i.e. its first span carries a `bg`. Used to fill the user
/// message bubble across the full viewport width so the highlight does not
/// stop at the end of the text.
pub(super) fn pad_bubble_line(line: Line<'static>, width: u16) -> Line<'static> {
    let bg: Option<Color> = line.spans.first().and_then(|s| s.style.bg);
    let Some(bg) = bg else {
        return line;
    };
    let cur = line.width() as u16;
    if cur >= width {
        return line;
    }
    let pad = " ".repeat((width - cur) as usize);
    let mut spans: Vec<Span<'static>> = line.spans;
    spans.push(Span::styled(pad, Style::default().bg(bg)));
    let mut out = Line::from(spans);
    out.style = line.style;
    out.alignment = line.alignment;
    out
}

impl ReactTrace {
    /// Build the flat sequence of display lines produced by the trace,
    /// before wrapping. Shared between `render` and `build_virtual_rows`.
    ///
    /// `body_width` is the available body width (terminal columns) used to
    /// size markdown tables so they pick the narrow-terminal records fallback
    /// instead of emitting a full-width grid that the caller's
    /// `wrap_line_to_width` would later split mid-border. Pass
    /// `Some(width.saturating_sub(3))` to account for the 3-space body indent;
    /// `None` disables width-aware table sizing (test helpers only).
    ///
    /// All returned lines have `'static` content.
    pub(super) fn build_display_lines(
        &self,
        spinner_frame: &str,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
        body_width: Option<u16>,
    ) -> Vec<Line<'static>> {
        let theme = &self.theme;
        let tok = |name: &str| resolve_token(theme, name, ColorDepth::Truecolor);
        let timestamp_color = tok("react_trace.timestamp.fg");
        let think_color = tok("react_trace.think.fg");
        let message_title_color = tok("react_trace.message.title.fg");
        let message_body_color = tok("react_trace.message.body.fg");
        let user_color = tok("react_trace.user_message.fg");
        let user_bg = tok("react_trace.user_message.bg");
        let user_accent_color = tok("react_trace.user_message.accent.fg");
        let permission_color = tok("react_trace.permission.fg");
        let spinner_color = tok("react_trace.spinner.fg");
        let success_color = tok("react_trace.outcome.success.fg");
        let error_color = tok("react_trace.outcome.error.fg");
        let observe_color = tok("react_trace.observe.fg");
        let delegate_color = tok("react_trace.delegate.fg");
        let collapsed = self.observe_collapsed;
        let mut lines: Vec<Line<'static>> = Vec::new();

        let mut i = 0;
        while i < self.entries.len() {
            let entry = &self.entries[i];
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(timestamp_color),
            );

            // Collapsed mode: render Act as a one-line summary.
            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } = &entry.kind
                {
                    let (act_glyph, act_color) = family_glyph(theme, *family);
                    let id_str = input_summary(input, tool);
                    let mut spans = vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", act_glyph, id_str),
                            Style::default().fg(act_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                    ];
                    match status {
                        ActStatus::Pending | ActStatus::InProgress { .. } => {
                            spans.push(Span::styled(
                                spinner_frame.to_string(),
                                Style::default().fg(spinner_color),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(theme, p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(timestamp_color),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✓".to_string(),
                                Style::default()
                                    .fg(success_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "✗".to_string(),
                                Style::default()
                                    .fg(error_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    lines.push(Line::from(spans));
                    lines.push(Line::from(""));
                    i += 1;
                    continue;
                }
            }

            match &entry.kind {
                TraceKind::Think => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "🧠 THINK",
                            Style::default()
                                .fg(think_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                terminal_safe_text(text_line),
                                Style::default().fg(think_color),
                            ),
                        ]));
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("📨 {}", agent),
                            Style::default()
                                .fg(message_title_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));

                    #[cfg(feature = "markdown")]
                    {
                        if let Some(stream) = entry.markdown.as_ref() {
                            let empty_state: std::collections::HashMap<
                                crate::components::mermaid::MermaidId,
                                crate::components::mermaid::FenceRender,
                            > = std::collections::HashMap::new();
                            render_agent_message_body(
                                stream,
                                &empty_state,
                                body_width,
                                |line| lines.push(line),
                                |_id, _h| unreachable!("secondary path passes empty fence_state"),
                            );
                        } else {
                            for text_line in entry.text.lines() {
                                lines.push(Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(message_body_color),
                                    ),
                                ]));
                            }
                        }
                    }

                    #[cfg(not(feature = "markdown"))]
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                terminal_safe_text(text_line),
                                Style::default().fg(message_body_color),
                            ),
                        ]));
                    }
                }

                TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } => {
                    let (glyph, glyph_color) = family_glyph(theme, *family);
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", glyph, tool),
                            Style::default()
                                .fg(glyph_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if matches!(input, spur_acp::adapter::ToolInputDisplay::Empty) {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(glyph_color),
                                ),
                            ]));
                        }
                    } else {
                        lines.extend(input_display_lines(theme, input));
                    }
                    // Render output inline from `status` — no paired Observe
                    // entry exists in the new model. In-progress tools keep
                    // their spinner while exposing the latest partial output.
                    match status {
                        ActStatus::InProgress { partial: Some(p) } => {
                            lines.extend(observe_payload_lines(theme, p, collapsed));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(theme, p);
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("{} {}", og, verb),
                                    Style::default().fg(oc).add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(theme, p, collapsed));
                        }
                        ActStatus::Failed(Some(p)) => {
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("✗ {}", verb),
                                    Style::default()
                                        .fg(error_color)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(theme, p, collapsed));
                        }
                        ActStatus::Completed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "✓ done".to_string(),
                                    Style::default()
                                        .fg(success_color)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Failed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "✗ failed".to_string(),
                                    Style::default()
                                        .fg(error_color)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Pending | ActStatus::InProgress { partial: None } => {}
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, glyph_color) = outcome_glyph(theme, p);
                        let verb = observe_verb(p);
                        lines.push(Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("{} {}", glyph, verb),
                                Style::default()
                                    .fg(glyph_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        lines.extend(observe_payload_lines(theme, p, collapsed));
                    } else {
                        lines.push(Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "👁 OBSERVE",
                                Style::default()
                                    .fg(observe_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(observe_color),
                                ),
                            ]));
                        }
                    }
                }

                TraceKind::Delegate {
                    agent,
                    task,
                    status,
                    request_id: _,
                    executor_id,
                } => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("→ DELEGATE to {}", agent),
                            Style::default()
                                .fg(delegate_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if !task.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(task.clone(), Style::default().fg(delegate_color)),
                        ]));
                    }
                    let effective_status = derive_delegate_status(executor_id.as_deref(), lineage)
                        .unwrap_or(status.as_str());
                    if !effective_status.is_empty() {
                        let is_active =
                            effective_status == "spawning" || effective_status == "running";
                        let status_text = if is_active {
                            format!("   {} {}", spinner_frame, effective_status)
                        } else {
                            format!("   {}", effective_status)
                        };
                        lines.push(Line::from(vec![Span::styled(
                            status_text,
                            Style::default().fg(delegate_color),
                        )]));
                    }
                    if let (Some(eid), Some(lin)) = (executor_id.as_ref(), lineage) {
                        let card_lines = crate::components::inline_executor_card::render_card(
                            lin,
                            &spur_core::ExecutorId(eid.clone()),
                            /* focused = */ false,
                        );
                        for line in card_lines {
                            lines.push(line);
                        }
                    }
                }

                TraceKind::UserMessage => {
                    let bar_style = Style::default()
                        .fg(user_accent_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    let ts_bubble_style = Style::default().fg(timestamp_color).bg(user_bg);
                    let label_style = Style::default()
                        .fg(user_accent_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    let body_style = Style::default()
                        .fg(user_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    lines.push(Line::from(vec![
                        Span::styled("▎ ", bar_style),
                        Span::styled(format!("{} ", entry.timestamp), ts_bubble_style),
                        Span::styled("💬 YOU", label_style),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled("▎ ", bar_style),
                            Span::styled("   ", body_style),
                            Span::styled(terminal_safe_text(text_line), body_style),
                        ]));
                    }
                }

                TraceKind::Permission {
                    description,
                    pending,
                    countdown,
                } => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("⚠ PERMISSION: {}", description),
                            Style::default()
                                .fg(permission_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if *pending {
                        let hint_text = if *countdown > 0 {
                            format!(
                                "   Select an advertised option (auto-cancel in {}s)",
                                countdown
                            )
                        } else {
                            "   Select an advertised option".to_string()
                        };
                        lines.push(Line::from(vec![Span::styled(
                            hint_text,
                            Style::default()
                                .fg(permission_color)
                                .add_modifier(Modifier::RAPID_BLINK),
                        )]));
                    }
                    if !entry.text.is_empty() {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(permission_color),
                                ),
                            ]));
                        }
                    }
                }

                TraceKind::Image { id, label } => {
                    let path_label = self
                        .inline_images
                        .get(id)
                        .map(|stored| stored.path.display().to_string())
                        .unwrap_or_else(|| entry.text.clone());
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("🖼 {}", label),
                            Style::default()
                                .fg(message_title_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if !entry.text.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                terminal_safe_text(&path_label),
                                Style::default().fg(message_body_color),
                            ),
                        ]));
                    }
                }
            }

            // Blank separator between entries. No adjacency skip needed: Act outcome
            // is now rendered from `status` inline, not from a neighbouring Observe.
            lines.push(Line::from(""));
            i += 1;
        }

        lines
    }
}

#[cfg(feature = "markdown")]
use crate::components::line_wrap::{wrap_line_to_width, wrap_line_to_width_with_char_starts};

#[cfg(feature = "markdown")]
use super::render::{PlainTextTailCache, VirtualRowCacheEntry};

#[cfg(feature = "markdown")]
use super::types::{InlineImageSource, VirtualRow};

#[cfg(feature = "markdown")]
use crate::components::markdown_stream::{MarkdownStream, StreamItem};

#[cfg(feature = "markdown")]
use crate::components::mermaid::{fence_placeholder_line, FenceRender, MermaidId};

#[cfg(feature = "markdown")]
pub(super) type VirtualRowsBuild = (
    Vec<VirtualRow>,
    Vec<usize>,
    Vec<Option<std::ops::Range<usize>>>,
    Option<PlainTextTailCache>,
);

/// Markdown punctuation that can retroactively change how an unfinished
/// paragraph is parsed. Keeping this predicate conservative makes the fast
/// path an optimization only; unsupported input always uses the established
/// preview/rebuild renderer.
#[cfg(feature = "markdown")]
fn is_plain_stream_text(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().any(|ch| {
            ch.is_control()
                || matches!(
                    ch,
                    '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '&' | '!' | '#' | '|'
                )
        })
}

/// Whether appending otherwise-plain bytes can no longer turn the beginning
/// of this paragraph into a Markdown block construct. A digit-only prefix can
/// still become an ordered list (`1.` / `1)`), while `-` and `+` can still
/// become unordered-list markers once a following space arrives.
#[cfg(feature = "markdown")]
fn has_append_stable_plain_prefix(text: &str) -> bool {
    text.trim_start_matches(' ')
        .chars()
        .next()
        .is_some_and(|ch| !ch.is_ascii_digit() && !matches!(ch, '-' | '+'))
}

#[cfg(feature = "markdown")]
fn plain_body_style(line: &Line<'_>, raw: &str) -> Option<Style> {
    let mut spans = line.spans.iter();
    let indent = spans.next()?;
    if indent.content.as_ref() != "   " {
        return None;
    }

    let first_body = spans.next()?;
    let body_style = first_body.style;
    let mut rendered = String::from(first_body.content.as_ref());
    for span in spans {
        if span.style != body_style {
            return None;
        }
        rendered.push_str(span.content.as_ref());
    }
    (rendered == raw.trim_end_matches(' ')).then_some(body_style)
}

/// Translate sorted character starts from an indented logical line into byte
/// starts in the raw body. `prefix_chars` accounts for the three-space body
/// indent on the first visual suffix only.
#[cfg(feature = "markdown")]
fn raw_byte_starts(
    raw: &str,
    line_char_starts: &[usize],
    prefix_chars: usize,
) -> Option<Vec<usize>> {
    let mut targets = Vec::with_capacity(line_char_starts.len());
    for (idx, start) in line_char_starts.iter().copied().enumerate() {
        if idx > 0 && start < prefix_chars {
            return None;
        }
        targets.push(start.saturating_sub(prefix_chars));
    }
    if !targets.windows(2).all(|pair| pair[0] <= pair[1]) {
        return None;
    }

    let mut bytes = vec![0; targets.len()];
    let mut next = 0usize;
    let mut char_count = 0usize;
    for (char_idx, (byte_idx, _)) in raw.char_indices().enumerate() {
        char_count = char_idx + 1;
        while next < targets.len() && targets[next] == char_idx {
            bytes[next] = byte_idx;
            next += 1;
        }
    }
    while next < targets.len() && targets[next] == char_count {
        bytes[next] = raw.len();
        next += 1;
    }
    (next == targets.len()).then_some(bytes)
}

#[cfg(feature = "markdown")]
fn push_plain_wrapped_rows(
    rows: &mut Vec<VirtualRow>,
    byte_ranges: &mut Vec<Option<std::ops::Range<usize>>>,
    line: Line<'static>,
    raw: &str,
    raw_base: usize,
    prefix_chars: usize,
    effective_width: u16,
) -> Option<usize> {
    let (wrapped, char_starts) = wrap_line_to_width_with_char_starts(&line, effective_width);
    let byte_starts = raw_byte_starts(raw, &char_starts, prefix_chars)?;
    if wrapped.is_empty() || byte_starts.len() != wrapped.len() {
        return None;
    }

    for (idx, wrapped_line) in wrapped.into_iter().enumerate() {
        let start = raw_base + byte_starts[idx];
        let end = byte_starts
            .get(idx + 1)
            .map_or(raw_base + raw.len(), |next| raw_base + *next);
        if start > end {
            return None;
        }
        rows.push(VirtualRow::Text(pad_bubble_line(
            wrapped_line,
            effective_width,
        )));
        byte_ranges.push(Some(start..end));
    }
    Some(rows.len() - 1)
}

/// Rebuild only the prior EOF-flushed visual row plus newly appended bytes.
/// Greedy wrapping makes every earlier row prefix-stable: each was committed
/// by an actual overflow, while only the final row was emitted because input
/// ended.
#[cfg(feature = "markdown")]
pub(super) fn reuse_plain_text_tail(
    entry: &super::types::TraceEntry,
    dirty_idx: usize,
    effective_width: u16,
    append_base_generation: Option<u64>,
    generation: u64,
    cache: &mut VirtualRowCacheEntry,
) -> bool {
    let Some(tail) = cache.plain_text_tail.clone() else {
        return false;
    };
    if effective_width <= 3
        || dirty_idx != tail.entry_idx
        || append_base_generation != Some(cache.generation)
        || generation == cache.generation
    {
        return false;
    }
    let TraceKind::AgentMessage { .. } = &entry.kind else {
        return false;
    };
    let Some(stream) = entry.markdown.as_ref() else {
        return false;
    };
    if stream.is_finalized() {
        return false;
    }

    let raw = stream.raw_text();
    if raw.len() <= tail.raw_len
        || tail.raw_tail_start > tail.raw_len
        || !raw.is_char_boundary(tail.raw_len)
        || !raw.is_char_boundary(tail.raw_tail_start)
        || !is_plain_stream_text(&raw[tail.raw_len..])
        || tail.mutable_row >= cache.rows.len()
        || tail.mutable_row >= cache.byte_ranges.len()
        || tail.entry_idx >= cache.entry_row_starts.len()
    {
        return false;
    }
    if cache.byte_ranges[tail.mutable_row]
        .as_ref()
        .is_none_or(|range| range.start != tail.raw_tail_start || range.end != tail.raw_len)
    {
        return false;
    }

    let suffix = &raw[tail.raw_tail_start..];
    // pulldown-cmark removes ASCII whitespace at EOF. Preserve its display
    // semantics while byte ranges continue to cover the unmodified raw text.
    let display_suffix = suffix.trim_end_matches(' ');
    let prefix_chars = if tail.raw_tail_start == 0 { 3 } else { 0 };
    let mut spans = Vec::with_capacity(2);
    if prefix_chars > 0 {
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled(display_suffix.to_string(), tail.body_style));
    let mut suffix_line = Line::from(spans);
    suffix_line.style = tail.line_style;
    suffix_line.alignment = tail.alignment;

    cache.rows.truncate(tail.mutable_row);
    cache.byte_ranges.truncate(tail.mutable_row);
    let Some(new_mutable_row) = push_plain_wrapped_rows(
        &mut cache.rows,
        &mut cache.byte_ranges,
        suffix_line,
        suffix,
        tail.raw_tail_start,
        prefix_chars,
        effective_width,
    ) else {
        return false;
    };

    // The header is intentionally coarse and may still cover the whole body.
    // Refresh only such coarse prefix ranges; completed body-row ranges remain
    // byte-stable and are the reusable invariant.
    let entry_start = cache.entry_row_starts[tail.entry_idx];
    for range in &mut cache.byte_ranges[entry_start..new_mutable_row] {
        if range.as_ref() == Some(&(0..tail.raw_len)) {
            *range = Some(0..raw.len());
        }
    }

    cache.rows.push(VirtualRow::Text(Line::from("")));
    cache.byte_ranges.push(None);
    cache.generation = generation;
    cache.plain_text_tail = Some(PlainTextTailCache {
        entry_idx: tail.entry_idx,
        mutable_row: new_mutable_row,
        raw_tail_start: cache.byte_ranges[new_mutable_row]
            .as_ref()
            .map_or(tail.raw_tail_start, |range| range.start),
        raw_len: raw.len(),
        reuse_count: tail.reuse_count.saturating_add(1),
        body_style: tail.body_style,
        line_style: tail.line_style,
        alignment: tail.alignment,
    });
    true
}

/// Render an AgentMessage body via [`MarkdownStream::preview_items`].
///
/// `preview_items` returns the same `Vec<StreamItem>` that `flush_final` would
/// produce, so tail bytes receive the same paragraph context they will have
/// after the final flush. This eliminates the row-count delta that caused ghost
/// text (RCA Layer 2A, fix design F1 / cursor-split renderer).
///
/// Two emit closures handle the two item kinds:
/// - `emit_line` — called for every [`StreamItem::Text`] row, and for fence
///   placeholders when the fence is not yet `Ready`.
/// - `emit_fence_image` — called for [`StreamItem::Fence`] entries whose
///   [`FenceRender`] is `Ready` with a non-zero pixel height.
#[cfg(feature = "markdown")]
fn render_agent_message_body(
    stream: &MarkdownStream,
    fence_state: &std::collections::HashMap<MermaidId, FenceRender>,
    body_width: Option<u16>,
    mut emit_line: impl FnMut(ratatui::text::Line<'static>),
    mut emit_fence_image: impl FnMut(MermaidId, u16),
) {
    use ratatui::text::{Line, Span};

    use std::collections::HashSet;
    let mut errors: HashSet<crate::components::mermaid::MermaidId> = HashSet::new();
    let mut pending: HashSet<crate::components::mermaid::MermaidId> = HashSet::new();
    for (id, render) in fence_state {
        match render {
            crate::components::mermaid::FenceRender::Error => {
                errors.insert(*id);
            }
            crate::components::mermaid::FenceRender::Pending => {
                pending.insert(*id);
            }
            crate::components::mermaid::FenceRender::Ready(_)
            | crate::components::mermaid::FenceRender::ReadyText(_) => {}
        }
    }
    let state_lookup = crate::components::markdown_stream::StateLookup {
        errors: &errors,
        pending: &pending,
    };

    let items = if let Some(body_width) = body_width {
        stream.preview_items_with_width(&state_lookup, body_width)
    } else {
        stream.preview_items(&state_lookup)
    };

    for item in &items {
        match item {
            StreamItem::Text(text_lines) => {
                for line in text_lines {
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(line.spans.iter().cloned());
                    let mut new_line = Line::from(spans);
                    new_line.style = line.style;
                    new_line.alignment = line.alignment;
                    emit_line(new_line);
                }
            }
            StreamItem::Fence(id) => match fence_state.get(id) {
                Some(FenceRender::Ready(h)) if *h > 0 => {
                    emit_fence_image(*id, *h);
                }
                Some(FenceRender::ReadyText(text)) => {
                    for text_line in text.lines() {
                        emit_line(Line::from(vec![
                            Span::raw("   "),
                            Span::raw(text_line.to_string()),
                        ]));
                    }
                }
                other => {
                    let render = match other {
                        Some(FenceRender::Error) => FenceRender::Error,
                        _ => FenceRender::Pending,
                    };
                    let placeholder = fence_placeholder_line(*id, render);
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(placeholder.spans.iter().cloned());
                    let mut line = Line::from(spans);
                    line.style = placeholder.style;
                    line.alignment = placeholder.alignment;
                    emit_line(line);
                }
            },
        }
    }
}

#[cfg(feature = "markdown")]
impl ReactTrace {
    /// Items-aware virtual row builder. Walks entries directly, and for
    /// `AgentMessage` entries iterates the markdown stream's items so
    /// `StreamItem::Fence(id)` can be expanded into N `ImageRow` entries
    /// when the fence is `Ready(h)`, or fall back to a state-aware single-row
    /// placeholder (⏳ Pending, ⚠ Error, 📊 default) otherwise.
    ///
    /// Duplicates some entry-kind rendering logic with `build_display_lines`;
    /// future work can consolidate.
    #[cfg(test)]
    pub(crate) fn build_virtual_rows(
        &self,
        from: usize,
        effective_width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> (
        Vec<VirtualRow>,
        Vec<usize>,
        Vec<Option<std::ops::Range<usize>>>,
    ) {
        let (rows, entry_row_starts, byte_ranges, _) =
            self.build_virtual_rows_with_tail(from, effective_width, states, lineage);
        (rows, entry_row_starts, byte_ranges)
    }

    pub(super) fn build_virtual_rows_with_tail(
        &self,
        from: usize,
        effective_width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> VirtualRowsBuild {
        let theme = &self.theme;
        let tok = |name: &str| resolve_token(theme, name, ColorDepth::Truecolor);
        let timestamp_color = tok("react_trace.timestamp.fg");
        let think_color = tok("react_trace.think.fg");
        let message_title_color = tok("react_trace.message.title.fg");
        let message_body_color = tok("react_trace.message.body.fg");
        let user_color = tok("react_trace.user_message.fg");
        let user_bg = tok("react_trace.user_message.bg");
        let user_accent_color = tok("react_trace.user_message.accent.fg");
        let permission_color = tok("react_trace.permission.fg");
        let spinner_color = tok("react_trace.spinner.fg");
        let success_color = tok("react_trace.outcome.success.fg");
        let error_color = tok("react_trace.outcome.error.fg");
        let observe_color = tok("react_trace.observe.fg");
        let delegate_color = tok("react_trace.delegate.fg");
        let spinner_frame = spinner::frame(spinner::BRAILLE, self.tick_counter as u32);
        let collapsed = self.observe_collapsed;

        let mut rows: Vec<VirtualRow> = Vec::new();
        let mut entry_row_starts = vec![0; self.entries.len().saturating_sub(from)];
        let mut byte_ranges: Vec<Option<std::ops::Range<usize>>> = Vec::new();
        let mut plain_text_tail = None;

        // Helper: wrap a Line to effective_width and push each wrapped visual
        // line as a VirtualRow::Text.
        let push_wrapped = |rows: &mut Vec<VirtualRow>,
                            byte_ranges: &mut Vec<Option<std::ops::Range<usize>>>,
                            range: Option<std::ops::Range<usize>>,
                            line: Line<'static>| {
            for w in wrap_line_to_width(&line, effective_width) {
                let spans: Vec<Span<'static>> = w
                    .spans
                    .into_iter()
                    .map(|s| Span::styled(s.content.into_owned(), s.style))
                    .collect();
                let mut out = Line::from(spans);
                out.style = w.style;
                out.alignment = w.alignment;
                let out = pad_bubble_line(out, effective_width);
                rows.push(VirtualRow::Text(out));
                byte_ranges.push(range.clone());
            }
        };

        let mut i = from;
        while i < self.entries.len() {
            entry_row_starts[i - from] = rows.len();
            let entry = &self.entries[i];
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(timestamp_color),
            );

            // Collapsed mode: render Act as a one-line summary.
            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } = &entry.kind
                {
                    let (act_glyph, act_color) = family_glyph(theme, *family);
                    let id_str = input_summary(input, tool);
                    let mut spans = vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", act_glyph, id_str),
                            Style::default().fg(act_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                    ];
                    match status {
                        ActStatus::Pending | ActStatus::InProgress { .. } => {
                            spans.push(Span::styled(
                                spinner_frame.to_string(),
                                Style::default().fg(spinner_color),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(theme, p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(timestamp_color),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✓".to_string(),
                                Style::default()
                                    .fg(success_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "✗".to_string(),
                                Style::default()
                                    .fg(error_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        Some(0..entry.text.len()),
                        Line::from(spans),
                    );
                    push_wrapped(&mut rows, &mut byte_ranges, None, Line::from(""));
                    i += 1;
                    continue;
                }
            }

            // Compute the byte length for this entry's content rows (coarse v1).
            let entry_byte_len = match &entry.kind {
                TraceKind::AgentMessage { .. } => entry
                    .markdown
                    .as_ref()
                    .map(|s| s.raw_text().len())
                    .unwrap_or(entry.text.len()),
                _ => entry.text.len(),
            };
            let content_range = Some(0..entry_byte_len);

            match &entry.kind {
                TraceKind::Think => {
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "🧠 THINK",
                                Style::default()
                                    .fg(think_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    for text_line in entry.text.lines() {
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(think_color),
                                ),
                            ]),
                        );
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("📨 {}", agent),
                                Style::default()
                                    .fg(message_title_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );

                    if let Some(stream) = entry.markdown.as_ref() {
                        // Collect body output into a staging area to avoid
                        // simultaneous mutable borrows of `rows` across the
                        // two emit closures required by render_agent_message_body.
                        enum BodyRow {
                            Line(Line<'static>),
                            Image { id: MermaidId, h: u16 },
                        }
                        let staged = std::cell::RefCell::new(Vec::<BodyRow>::new());
                        render_agent_message_body(
                            stream,
                            states,
                            Some(effective_width.saturating_sub(3)),
                            |line| staged.borrow_mut().push(BodyRow::Line(line)),
                            |id, h| staged.borrow_mut().push(BodyRow::Image { id, h }),
                        );
                        let mut staged = staged.into_inner();
                        let raw = stream.raw_text();
                        let plain_style = if effective_width > 3
                            && i + 1 == self.entries.len()
                            && !stream.is_finalized()
                            && is_plain_stream_text(raw)
                            && has_append_stable_plain_prefix(raw)
                            && staged.len() == 1
                        {
                            match &staged[0] {
                                BodyRow::Line(line) => plain_body_style(line, raw)
                                    .map(|style| (style, line.style, line.alignment)),
                                BodyRow::Image { .. } => None,
                            }
                        } else {
                            None
                        };

                        if let Some((body_style, line_style, alignment)) = plain_style {
                            let BodyRow::Line(line) = staged.pop().unwrap() else {
                                unreachable!("plain body candidate is always a text row")
                            };
                            if let Some(mutable_row) = push_plain_wrapped_rows(
                                &mut rows,
                                &mut byte_ranges,
                                line.clone(),
                                raw,
                                0,
                                3,
                                effective_width,
                            ) {
                                let raw_tail_start = byte_ranges[mutable_row]
                                    .as_ref()
                                    .map_or(0, |range| range.start);
                                plain_text_tail = Some(PlainTextTailCache {
                                    entry_idx: i,
                                    mutable_row,
                                    raw_tail_start,
                                    raw_len: raw.len(),
                                    reuse_count: 0,
                                    body_style,
                                    line_style,
                                    alignment,
                                });
                                staged.clear();
                            } else {
                                staged.push(BodyRow::Line(line));
                            }
                        }

                        for item in staged {
                            match item {
                                BodyRow::Line(line) => push_wrapped(
                                    &mut rows,
                                    &mut byte_ranges,
                                    content_range.clone(),
                                    line,
                                ),
                                BodyRow::Image { id, h } => {
                                    for r in 0..h {
                                        rows.push(VirtualRow::ImageRow {
                                            source: InlineImageSource::Mermaid(id),
                                            row_within: r,
                                            total_rows: h,
                                        });
                                        byte_ranges.push(content_range.clone());
                                    }
                                }
                            }
                        }
                    } else {
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(message_body_color),
                                    ),
                                ]),
                            );
                        }
                    }
                }

                TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } => {
                    let (glyph, glyph_color) = family_glyph(theme, *family);
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("{} {}", glyph, tool),
                                Style::default()
                                    .fg(glyph_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if matches!(input, spur_acp::adapter::ToolInputDisplay::Empty) {
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(glyph_color),
                                    ),
                                ]),
                            );
                        }
                    } else {
                        for line in input_display_lines(theme, input) {
                            push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), line);
                        }
                    }
                    match status {
                        ActStatus::InProgress { partial: Some(p) } => {
                            for l in observe_payload_lines(theme, p, collapsed) {
                                push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), l);
                            }
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(theme, p);
                            let verb = observe_verb(p);
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        format!("{} {}", og, verb),
                                        Style::default().fg(oc).add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                            for l in observe_payload_lines(theme, p, collapsed) {
                                push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), l);
                            }
                        }
                        ActStatus::Failed(Some(p)) => {
                            let verb = observe_verb(p);
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        format!("✗ {}", verb),
                                        Style::default()
                                            .fg(error_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                            for l in observe_payload_lines(theme, p, collapsed) {
                                push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), l);
                            }
                        }
                        ActStatus::Completed(None) => {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        "✓ done".to_string(),
                                        Style::default()
                                            .fg(success_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                        }
                        ActStatus::Failed(None) => {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        "✗ failed".to_string(),
                                        Style::default()
                                            .fg(error_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                        }
                        ActStatus::Pending | ActStatus::InProgress { partial: None } => {}
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, glyph_color) = outcome_glyph(theme, p);
                        let verb = observe_verb(p);
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("{} {}", glyph, verb),
                                    Style::default()
                                        .fg(glyph_color)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]),
                        );
                        for line in observe_payload_lines(theme, p, collapsed) {
                            push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), line);
                        }
                    } else {
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "👁 OBSERVE",
                                    Style::default()
                                        .fg(observe_color)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]),
                        );
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(observe_color),
                                    ),
                                ]),
                            );
                        }
                    }
                }

                TraceKind::Delegate {
                    agent,
                    task,
                    status,
                    request_id: _,
                    executor_id,
                } => {
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("→ DELEGATE to {}", agent),
                                Style::default()
                                    .fg(delegate_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if !task.is_empty() {
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(task.clone(), Style::default().fg(delegate_color)),
                            ]),
                        );
                    }
                    let effective_status = derive_delegate_status(executor_id.as_deref(), lineage)
                        .unwrap_or(status.as_str());
                    if !effective_status.is_empty() {
                        let is_active =
                            effective_status == "spawning" || effective_status == "running";
                        let status_text = if is_active {
                            format!("   {} {}", spinner_frame, effective_status)
                        } else {
                            format!("   {}", effective_status)
                        };
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![Span::styled(
                                status_text,
                                Style::default().fg(delegate_color),
                            )]),
                        );
                    }
                    if let (Some(eid), Some(lin)) = (executor_id.as_ref(), lineage) {
                        let card_lines = crate::components::inline_executor_card::render_card(
                            lin,
                            &spur_core::ExecutorId(eid.clone()),
                            /* focused = */ false,
                        );
                        for line in card_lines {
                            push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), line);
                        }
                    }
                }

                TraceKind::UserMessage => {
                    let bar_style = Style::default()
                        .fg(user_accent_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    let ts_bubble_style = Style::default().fg(timestamp_color).bg(user_bg);
                    let label_style = Style::default()
                        .fg(user_accent_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    let body_style = Style::default()
                        .fg(user_color)
                        .bg(user_bg)
                        .add_modifier(Modifier::BOLD);
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            Span::styled("▎ ", bar_style),
                            Span::styled(format!("{} ", entry.timestamp), ts_bubble_style),
                            Span::styled("💬 YOU", label_style),
                        ]),
                    );
                    for text_line in entry.text.lines() {
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![
                                Span::styled("▎ ", bar_style),
                                Span::styled("   ", body_style),
                                Span::styled(terminal_safe_text(text_line), body_style),
                            ]),
                        );
                    }
                }

                TraceKind::Permission {
                    description,
                    pending,
                    countdown,
                } => {
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("⚠ PERMISSION: {}", description),
                                Style::default()
                                    .fg(permission_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if *pending {
                        let hint_text = if *countdown > 0 {
                            format!(
                                "   Select an advertised option (auto-cancel in {}s)",
                                countdown
                            )
                        } else {
                            "   Select an advertised option".to_string()
                        };
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![Span::styled(
                                hint_text,
                                Style::default()
                                    .fg(permission_color)
                                    .add_modifier(Modifier::RAPID_BLINK),
                            )]),
                        );
                    }
                    if !entry.text.is_empty() {
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(permission_color),
                                    ),
                                ]),
                            );
                        }
                    }
                }

                TraceKind::Image { id, label } => {
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("🖼 {}", label),
                                Style::default()
                                    .fg(message_title_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    let image_rows = self
                        .inline_images
                        .get(id)
                        .map(|stored| {
                            crate::components::react_trace::render::compute_inline_height_rows(
                                stored.image.as_ref(),
                                effective_width,
                                60,
                                8,
                                16,
                            )
                        })
                        .unwrap_or(1);
                    for r in 0..image_rows {
                        rows.push(VirtualRow::ImageRow {
                            source: InlineImageSource::Trace(*id),
                            row_within: r,
                            total_rows: image_rows,
                        });
                        byte_ranges.push(content_range.clone());
                    }
                }
            }

            // Blank separator between entries. No adjacency skip needed: Act outcome
            // is now rendered from `status` inline, not from a neighbouring Observe.
            push_wrapped(&mut rows, &mut byte_ranges, None, Line::from(""));
            i += 1;
        }

        (rows, entry_row_starts, byte_ranges, plain_text_tail)
    }
}
