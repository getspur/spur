use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::components::trace_format::{
    derive_delegate_status, family_glyph, input_display_lines, input_summary, observe_compact,
    observe_payload_lines, observe_verb, outcome_glyph,
};

use super::types::TraceKind;
use super::ReactTrace;
use super::SPINNER_FRAMES;

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
                    let consumed = if let Some(TraceKind::Observe { payload: Some(p) }) =
                        self.entries.get(i + 1).map(|e| &e.kind)
                    {
                        let (obs_glyph, obs_color, stats) = observe_compact(p);
                        spans.push(Span::styled(
                            obs_glyph.to_string(),
                            Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                        ));
                        if !stats.is_empty() {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(stats, Style::default().fg(Color::DarkGray)));
                        }
                        2
                    } else {
                        spans.push(Span::styled(
                            spinner_frame.to_string(),
                            Style::default().fg(Color::Yellow),
                        ));
                        1
                    };
                    lines.push(Line::from(spans));
                    lines.push(Line::from(""));
                    i += consumed;
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
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                text_line.to_string(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("✉ {}", agent),
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
                                |_id, _h| {
                                    unreachable!("secondary path passes empty fence_state")
                                },
                            );
                        } else {
                            for text_line in entry.text.lines() {
                                lines.push(Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        text_line.to_string(),
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
                            Span::styled(
                                text_line.to_string(),
                                Style::default().fg(Color::White),
                            ),
                        ]));
                    }
                }

                TraceKind::Act {
                    tool,
                    family,
                    input,
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
                                    text_line.to_string(),
                                    Style::default().fg(glyph_color),
                                ),
                            ]));
                        }
                    } else {
                        lines.extend(input_display_lines(input));
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
                                "👁 OBSERVE",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
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
                            format!("→ DELEGATE to {}", agent),
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
                        .unwrap_or_else(|| status.as_str());
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
                            "💬 YOU",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(text_line.to_string(), Style::default().fg(Color::Yellow)),
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
                                    text_line.to_string(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                }
            }

            let skip_blank = matches!(&entry.kind, TraceKind::Act { .. })
                && matches!(
                    self.entries.get(i + 1).map(|e| &e.kind),
                    Some(TraceKind::Observe { payload: Some(_) })
                );
            if !skip_blank {
                lines.push(Line::from(""));
            }
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
use crate::components::mermaid::{FenceRender, MermaidId, fence_placeholder_line};

/// Render an AgentMessage body via the cursor-split contract.
///
/// Emits:
/// 1. Committed items from `stream.items_and_tail().0` — styled text and
///    fence rows (image via `emit_fence_image`, placeholder via `emit_line`).
/// 2. The uncommitted tail from `stream.items_and_tail().1` — plain white
///    lines with the 3-space indent.
///
/// The two-closure split lets the primary render path emit multiple
/// `VirtualRow::ImageRow` entries per mermaid fence while the secondary
/// path renders a single placeholder line (no ImageRow concept).
#[cfg(feature = "markdown")]
fn render_agent_message_body(
    stream: &MarkdownStream,
    fence_state: &std::collections::HashMap<MermaidId, FenceRender>,
    mut emit_line: impl FnMut(ratatui::text::Line<'static>),
    mut emit_fence_image: impl FnMut(MermaidId, u16),
) {
    use ratatui::{
        style::{Color, Style},
        text::{Line, Span},
    };

    let (items, tail) = stream.items_and_tail();

    for item in items {
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

    // Plain-text tail, indented, white.
    for text_line in tail.lines() {
        emit_line(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                text_line.to_string(),
                Style::default().fg(Color::White),
            ),
        ]));
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
    ) -> (Vec<VirtualRow>, Vec<usize>) {
        let spinner_frame = SPINNER_FRAMES[(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];
        let collapsed = self.observe_collapsed;

        let mut rows: Vec<VirtualRow> = Vec::new();
        let mut entry_row_starts = vec![0; self.entries.len().saturating_sub(from)];

        // Helper: wrap a Line to effective_width and push each wrapped visual
        // line as a VirtualRow::Text.
        let push_wrapped = |rows: &mut Vec<VirtualRow>, line: Line<'static>| {
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
            }
        };

        let mut i = from;
        // When starting mid-trace in collapsed mode, skip an Observe that
        // was consumed by the preceding Act.
        if collapsed && i > 0 {
            if matches!(&self.entries.get(i).map(|e| &e.kind), Some(TraceKind::Observe { payload: Some(_) })) {
                if matches!(&self.entries.get(i - 1).map(|e| &e.kind), Some(TraceKind::Act { .. })) {
                    i += 1;
                }
            }
        }
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
                    let consumed = if let Some(TraceKind::Observe { payload: Some(p) }) =
                        self.entries.get(i + 1).map(|e| &e.kind)
                    {
                        let (obs_glyph, obs_color, stats) = observe_compact(p);
                        spans.push(Span::styled(
                            obs_glyph.to_string(),
                            Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
                        ));
                        if !stats.is_empty() {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(stats, Style::default().fg(Color::DarkGray)));
                        }
                        2
                    } else {
                        spans.push(Span::styled(
                            spinner_frame.to_string(),
                            Style::default().fg(Color::Yellow),
                        ));
                        1
                    };
                    push_wrapped(&mut rows, Line::from(spans));
                    push_wrapped(&mut rows, Line::from(""));
                    if consumed == 2 && i + 1 >= from {
                        entry_row_starts[i + 1 - from] = rows.len();
                    }
                    i += consumed;
                    continue;
                }
            }

            match &entry.kind {
                TraceKind::Think => {
                    push_wrapped(
                        &mut rows,
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "🧠 THINK",
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    for text_line in entry.text.lines() {
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]),
                        );
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    push_wrapped(
                        &mut rows,
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("✉ {}", agent),
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
                                BodyRow::Line(line) => push_wrapped(&mut rows, line),
                                BodyRow::Image { id, h } => {
                                    for r in 0..h {
                                        rows.push(VirtualRow::ImageRow {
                                            id,
                                            row_within: r,
                                            total_rows: h,
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        text_line.to_string(),
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
                } => {
                    let (glyph, glyph_color) = family_glyph(*family);
                    push_wrapped(
                        &mut rows,
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
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        text_line.to_string(),
                                        Style::default().fg(glyph_color),
                                    ),
                                ]),
                            );
                        }
                    } else {
                        for line in input_display_lines(input) {
                            push_wrapped(&mut rows, line);
                        }
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, glyph_color) = outcome_glyph(p);
                        let verb = observe_verb(p);
                        push_wrapped(
                            &mut rows,
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
                            push_wrapped(&mut rows, line);
                        }
                    } else {
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "👁 OBSERVE",
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]),
                        );
                        for text_line in entry.text.lines() {
                            push_wrapped(
                                &mut rows,
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        text_line.to_string(),
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
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("→ DELEGATE to {}", agent),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if !task.is_empty() {
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(task.clone(), Style::default().fg(Color::Cyan)),
                            ]),
                        );
                    }
                    let effective_status = derive_delegate_status(executor_id.as_deref(), lineage)
                        .unwrap_or_else(|| status.as_str());
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
                            push_wrapped(&mut rows, line);
                        }
                    }
                }

                TraceKind::UserMessage => {
                    push_wrapped(
                        &mut rows,
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                "💬 YOU",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    for text_line in entry.text.lines() {
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
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
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("⚠ PERMISSION: {}", description),
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
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        text_line.to_string(),
                                        Style::default().fg(Color::Yellow),
                                    ),
                                ]),
                            );
                        }
                    }
                }
            }

            let skip_blank = matches!(&entry.kind, TraceKind::Act { .. })
                && matches!(
                    self.entries.get(i + 1).map(|e| &e.kind),
                    Some(TraceKind::Observe { payload: Some(_) })
                );
            if !skip_blank {
                push_wrapped(&mut rows, Line::from(""));
            }
            i += 1;
        }

        (rows, entry_row_starts)
    }
}
