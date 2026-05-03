use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::components::trace_format::{
    derive_delegate_status, family_glyph, input_display_lines, input_summary, observe_compact,
    observe_payload_lines, observe_verb, outcome_glyph, terminal_safe_text,
};

use super::types::{ActStatus, TraceKind};
use super::ReactTrace;
use crate::components::spinner;

impl ReactTrace {
    /// Build the flat sequence of display lines produced by the trace,
    /// before wrapping. Shared between `render` and `build_virtual_rows`.
    ///
    /// All returned lines have `'static` content.
    pub(super) fn build_display_lines(
        &self,
        spinner_frame: &str,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> Vec<Line<'static>> {
        let collapsed = self.observe_collapsed;
        let mut lines: Vec<Line<'static>> = Vec::new();

        let mut i = 0;
        while i < self.entries.len() {
            let entry = &self.entries[i];
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(Color::DarkGray),
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
                    let (act_glyph, act_color) = family_glyph(*family);
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
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✅".to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "❌".to_string(),
                                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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
                            "THINK",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                terminal_safe_text(text_line),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("MSG {}", agent),
                            Style::default()
                                .fg(Color::Cyan)
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
                                |line| lines.push(line),
                                |_id, _h| unreachable!("secondary path passes empty fence_state"),
                            );
                        } else {
                            for text_line in entry.text.lines() {
                                lines.push(Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        terminal_safe_text(text_line),
                                        Style::default().fg(Color::White),
                                    ),
                                ]));
                            }
                        }
                    }

                    #[cfg(not(feature = "markdown"))]
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(terminal_safe_text(text_line), Style::default().fg(Color::White)),
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
                    let (glyph, glyph_color) = family_glyph(*family);
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
                        lines.extend(input_display_lines(input));
                    }
                    // Render outcome body inline from `status` — no paired
                    // Observe entry exists in the new model.
                    match status {
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(p);
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("{} {}", og, verb),
                                    Style::default().fg(oc).add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(p, collapsed));
                        }
                        ActStatus::Failed(Some(p)) => {
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("err {}", verb),
                                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(p, collapsed));
                        }
                        ActStatus::Completed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "ok done".to_string(),
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Failed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "err failed".to_string(),
                                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Pending | ActStatus::InProgress { .. } => {}
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, glyph_color) = outcome_glyph(p);
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
                        lines.extend(observe_payload_lines(p, collapsed));
                    } else {
                        lines.push(Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "OBSERVE",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(Color::Green),
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
                            format!("DELEGATE to {}", agent),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if !task.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(task.clone(), Style::default().fg(Color::Cyan)),
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
                            Style::default().fg(Color::Cyan),
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
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "YOU",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(terminal_safe_text(text_line), Style::default().fg(Color::Yellow)),
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
                            format!("PERMISSION: {}", description),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if *pending {
                        let hint_text = if *countdown > 0 {
                            format!("   [y]es [n]o [a]lways  (auto-deny in {}s)", countdown)
                        } else {
                            "   [y]es [n]o [a]lways".to_string()
                        };
                        lines.push(Line::from(vec![Span::styled(
                            hint_text,
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::RAPID_BLINK),
                        )]));
                    }
                    if !entry.text.is_empty() {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    terminal_safe_text(text_line),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
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
use crate::components::line_wrap::wrap_line_to_width;

#[cfg(feature = "markdown")]
use super::types::VirtualRow;

#[cfg(feature = "markdown")]
use crate::components::markdown_stream::{MarkdownStream, StreamItem};

#[cfg(feature = "markdown")]
use crate::components::mermaid::{fence_placeholder_line, FenceRender, MermaidId};

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
            crate::components::mermaid::FenceRender::Ready(_) => {}
        }
    }
    let state_lookup = crate::components::markdown_stream::StateLookup {
        errors: &errors,
        pending: &pending,
    };

    let items = stream.preview_items(&state_lookup);

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
            StreamItem::Fence(id) => match fence_state.get(id).copied() {
                Some(FenceRender::Ready(h)) if h > 0 => {
                    emit_fence_image(*id, h);
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
        let spinner_frame = spinner::frame(spinner::BRAILLE, self.tick_counter as u32);
        let collapsed = self.observe_collapsed;

        let mut rows: Vec<VirtualRow> = Vec::new();
        let mut entry_row_starts = vec![0; self.entries.len().saturating_sub(from)];
        let mut byte_ranges: Vec<Option<std::ops::Range<usize>>> = Vec::new();

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
                Style::default().fg(Color::DarkGray),
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
                    let (act_glyph, act_color) = family_glyph(*family);
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
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✅".to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "❌".to_string(),
                                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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
                            "THINK",
                                Style::default()
                                    .fg(Color::DarkGray)
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
                                    Style::default().fg(Color::DarkGray),
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
                                format!("MSG {}", agent),
                                Style::default()
                                    .fg(Color::Cyan)
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
                            |line| staged.borrow_mut().push(BodyRow::Line(line)),
                            |id, h| staged.borrow_mut().push(BodyRow::Image { id, h }),
                        );
                        for item in staged.into_inner() {
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
                                            id,
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
                                        Style::default().fg(Color::White),
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
                    let (glyph, glyph_color) = family_glyph(*family);
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
                        for line in input_display_lines(input) {
                            push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), line);
                        }
                    }
                    match status {
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(p);
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
                            for l in observe_payload_lines(p, collapsed) {
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
                                        format!("err {}", verb),
                                        Style::default()
                                            .fg(Color::Red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                            for l in observe_payload_lines(p, collapsed) {
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
                                        "ok done".to_string(),
                                        Style::default()
                                            .fg(Color::Green)
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
                                        "err failed".to_string(),
                                        Style::default()
                                            .fg(Color::Red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                        }
                        ActStatus::Pending | ActStatus::InProgress { .. } => {}
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, glyph_color) = outcome_glyph(p);
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
                        for line in observe_payload_lines(p, collapsed) {
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
                                    "OBSERVE",
                                    Style::default()
                                        .fg(Color::Green)
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
                                        Style::default().fg(Color::Green),
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
                                format!("DELEGATE to {}", agent),
                                Style::default()
                                    .fg(Color::Cyan)
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
                                Span::styled(task.clone(), Style::default().fg(Color::Cyan)),
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
                                Style::default().fg(Color::Cyan),
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
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        content_range.clone(),
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "YOU",
                                Style::default()
                                    .fg(Color::Yellow)
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
                                    Style::default().fg(Color::Yellow),
                                ),
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
                                format!("PERMISSION: {}", description),
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if *pending {
                        let hint_text = if *countdown > 0 {
                            format!("   [y]es [n]o [a]lways  (auto-deny in {}s)", countdown)
                        } else {
                            "   [y]es [n]o [a]lways".to_string()
                        };
                        push_wrapped(
                            &mut rows,
                            &mut byte_ranges,
                            content_range.clone(),
                            Line::from(vec![Span::styled(
                                hint_text,
                                Style::default()
                                    .fg(Color::Yellow)
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
                                        Style::default().fg(Color::Yellow),
                                    ),
                                ]),
                            );
                        }
                    }
                }
            }

            // Blank separator between entries. No adjacency skip needed: Act outcome
            // is now rendered from `status` inline, not from a neighbouring Observe.
            push_wrapped(&mut rows, &mut byte_ranges, None, Line::from(""));
            i += 1;
        }

        (rows, entry_row_starts, byte_ranges)
    }
}
