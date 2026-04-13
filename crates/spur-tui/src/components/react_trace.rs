use std::cell::Cell;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::line_wrap::wrap_line_to_width;
use super::MAX_LOG_ENTRIES;

/// What kind of ReAct trace step this entry represents.
#[derive(Debug, Clone)]
pub enum TraceKind {
    Think,
    AgentMessage { agent: String },
    Act { tool: String, args: String },
    Observe,
    Delegate { agent: String, task: String, status: String },
    UserMessage,
    Permission { description: String, pending: bool, countdown: u8 },
}

/// A single entry in the full ReAct trace.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub kind: TraceKind,
    pub text: String,
    pub timestamp: String,
    /// Per-entry markdown renderer, populated only for `TraceKind::AgentMessage`
    /// when the `markdown` feature is enabled. `text` is kept in sync with the
    /// stream's `raw_text` so non-markdown rendering paths still work.
    #[cfg(feature = "markdown")]
    pub markdown: Option<super::markdown_stream::MarkdownStream>,
}

/// A flattened unit of rendered output: one visual row per variant.
/// Task 3 scope: every row is `Text`. Task 4 will add image-row expansion.
#[cfg(feature = "markdown")]
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields consumed by Task 6 (inline render migration).
pub(crate) enum VirtualRow {
    Text(Line<'static>),
    ImageRow {
        id: crate::components::mermaid::MermaidId,
        row_within: u16,
        total_rows: u16,
    },
}

/// Spinner frames for delegation animation.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct ReactTrace {
    entries: Vec<TraceEntry>,
    scroll_offset: usize,
    is_following: bool,
    tick_counter: u8,
    /// Cached total rendered lines from last render (interior mutability for &self render).
    last_total_lines: Cell<usize>,
    /// Cached visible height from last render.
    last_visible_height: Cell<usize>,
}

impl ReactTrace {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            is_following: true,
            tick_counter: 0,
            last_total_lines: Cell::new(0),
            last_visible_height: Cell::new(20),
        }
    }

    /// Return a short kind name for the most recent entry, or `None` if empty.
    ///
    /// Used by diagnostic logging to detect when a trace entry of a different
    /// kind sits between successive `AgentMessageChunk`s — which forces
    /// `append_message` to push a new block instead of continuing the previous
    /// one.
    pub fn last_entry_kind_name(&self) -> Option<&'static str> {
        self.entries.last().map(|e| match &e.kind {
            TraceKind::Think => "think",
            TraceKind::AgentMessage { .. } => "agent_message",
            TraceKind::Act { .. } => "act",
            TraceKind::Observe => "observe",
            TraceKind::Delegate { .. } => "delegate",
            TraceKind::UserMessage => "user_message",
            TraceKind::Permission { .. } => "permission",
        })
    }

    /// Append text to the most recent THINK entry, or create a new one
    /// if the last entry is not THINK. This prevents each TextDelta chunk
    /// from creating a separate "🧠 THINK" block in the trace.
    pub fn append_think(&mut self, text: &str, timestamp: String) {
        if let Some(last) = self.entries.last_mut() {
            if matches!(last.kind, TraceKind::Think) {
                // Append to existing THINK entry
                if !last.text.is_empty() {
                    last.text.push_str(text);
                } else {
                    last.text = text.to_string();
                }
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
        // No previous THINK entry — create a new one
        self.push(TraceEntry {
            kind: TraceKind::Think,
            text: text.to_string(),
            timestamp,
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Append text to the most recent AgentMessage entry, or create a new one.
    /// Same accumulation pattern as append_think but for agent responses.
    pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String) {
        #[cfg(feature = "markdown")]
        {
            if let Some(last) = self.entries.last_mut() {
                if let TraceKind::AgentMessage { .. } = last.kind {
                    if let Some(stream) = last.markdown.as_mut() {
                        stream.append(text);
                    }
                    if self.is_following {
                        self.scroll_to_bottom();
                    }
                    return;
                }
            }
            let mut stream = super::markdown_stream::MarkdownStream::new();
            stream.append(text);
            self.push(TraceEntry {
                kind: TraceKind::AgentMessage { agent: agent.to_string() },
                text: String::new(), // stream owns the raw text
                timestamp,
                markdown: Some(stream),
            });
            return;
        }
        #[cfg(not(feature = "markdown"))]
        {
            if let Some(last) = self.entries.last_mut() {
                if matches!(last.kind, TraceKind::AgentMessage { .. }) {
                    last.text.push_str(text);
                    if self.is_following {
                        self.scroll_to_bottom();
                    }
                    return;
                }
            }
            self.push(TraceEntry {
                kind: TraceKind::AgentMessage { agent: agent.to_string() },
                text: text.to_string(),
                timestamp,
            });
        }
    }

    /// Push a new trace entry, evicting oldest if over capacity, and
    /// auto-scroll to bottom when following.
    pub fn push(&mut self, entry: TraceEntry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            self.scroll_offset = self.scroll_offset.saturating_sub(drain);
        }
        if self.is_following {
            self.scroll_to_bottom();
        }
    }

    fn max_offset(&self) -> usize {
        self.last_total_lines
            .get()
            .saturating_sub(self.last_visible_height.get())
    }

    pub fn scroll_up(&mut self) {
        // When following, scroll_offset may be stale. Sync to real bottom first.
        if self.is_following {
            self.scroll_offset = self.max_offset();
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        if self.is_following {
            self.scroll_offset = self.max_offset();
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.is_following = false;
    }

    pub fn scroll_down(&mut self) {
        let max = self.max_offset();
        self.scroll_offset = self.scroll_offset.saturating_add(1).min(max);
        if self.scroll_offset >= max {
            self.is_following = true;
        }
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        let max = self.max_offset();
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max);
        if self.scroll_offset >= max {
            self.is_following = true;
        }
    }

    pub fn page_up(&mut self) {
        if self.is_following {
            self.scroll_offset = self.max_offset();
        }
        let jump = self.last_visible_height.get().saturating_sub(2).max(1);
        self.scroll_offset = self.scroll_offset.saturating_sub(jump);
        self.is_following = false;
    }

    pub fn page_down(&mut self) {
        let jump = self.last_visible_height.get().saturating_sub(2).max(1);
        let max = self.max_offset();
        self.scroll_offset = self.scroll_offset.saturating_add(jump).min(max);
        if self.scroll_offset >= max {
            self.is_following = true;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_offset();
        self.is_following = true;
    }

    /// Called on each tick: advance spinner counter and decrement pending
    /// permission countdowns.
    pub fn tick(&mut self) {
        self.tick_counter = self.tick_counter.wrapping_add(1);

        for entry in &mut self.entries {
            if let TraceKind::Permission {
                pending,
                countdown,
                ..
            } = &mut entry.kind
            {
                if *pending && *countdown > 0 {
                    *countdown = countdown.saturating_sub(1);
                }
            }
        }
    }

    /// Drain any mermaid fences detected during the last debounce window.
    /// Returns (entry_index, FenceRef) pairs. Empty if the `markdown` feature
    /// is disabled.
    ///
    /// `states` is passed through to the per-entry `maybe_flush` call so that
    /// rebuilt placeholders can reflect error vs pending state.
    #[cfg(feature = "markdown")]
    pub fn drain_fence_dispatches(
        &mut self,
        states: &super::markdown_stream::StateLookup<'_>,
    ) -> Vec<(usize, super::markdown_stream::FenceRef)> {
        let mut out = Vec::new();
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                for fence in stream.maybe_flush(states) {
                    out.push((idx, fence));
                }
            }
        }
        out
    }

    #[cfg(not(feature = "markdown"))]
    pub fn drain_fence_dispatches(&mut self) -> Vec<(usize, ())> {
        Vec::new()
    }

    /// Force an immediate rebuild of every markdown stream, bypassing the
    /// 50ms debounce. Returns fences newly discovered during the flush.
    /// Used on TurnComplete so the final chunk of a turn is rendered without
    /// the debounce-window lag.
    #[cfg(feature = "markdown")]
    pub fn force_flush_all(
        &mut self,
        states: &super::markdown_stream::StateLookup<'_>,
    ) -> Vec<(usize, super::markdown_stream::FenceRef)> {
        let mut out = Vec::new();
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                for fence in stream.flush_now(states) {
                    out.push((idx, fence));
                }
            }
        }
        out
    }

    #[cfg(not(feature = "markdown"))]
    pub fn force_flush_all(&mut self) -> Vec<(usize, ())> {
        Vec::new()
    }

    /// Mark every `AgentMessage` markdown stream as dirty so that the next
    /// `drain_fence_dispatches` / `maybe_flush` rebuilds all placeholders.
    /// Called when external state changes (e.g. a mermaid render error arrives)
    /// so the cached lines are refreshed even though the raw text is unchanged.
    #[cfg(feature = "markdown")]
    pub fn mark_all_streams_dirty(&mut self) {
        for entry in &mut self.entries {
            if let Some(stream) = entry.markdown.as_mut() {
                stream.mark_dirty_now();
            }
        }
    }

    /// Returns true if any entry has a pending permission request.
    pub fn has_pending_permission(&self) -> bool {
        self.entries.iter().any(|e| {
            matches!(
                &e.kind,
                TraceKind::Permission { pending: true, .. }
            )
        })
    }

    /// Mark all pending permission entries as resolved.
    pub fn resolve_pending_permissions(&mut self) {
        for entry in &mut self.entries {
            if let TraceKind::Permission { pending, .. } = &mut entry.kind {
                *pending = false;
            }
        }
    }

    /// Build the flat sequence of display lines produced by the trace,
    /// before wrapping. Shared between `render` and `build_virtual_rows`.
    ///
    /// All returned lines have `'static` content.
    fn build_display_lines(&self, spinner_frame: &str) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for entry in &self.entries {
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(Color::DarkGray),
            );

            match &entry.kind {
                TraceKind::Think => {
                    // Header line: timestamp + "🧠 THINK"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "🧠 THINK",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines indented 3 spaces
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
                    // Header line: timestamp + "✉ agent_name"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("✉ {}", agent),
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                    ]));

                    #[cfg(feature = "markdown")]
                    let used_markdown = entry
                        .markdown
                        .as_ref()
                        .filter(|stream| !stream.items().is_empty())
                        .map(|stream| {
                            for line in stream.lines() {
                                // Indent markdown lines by 3 spaces to match existing format.
                                let mut spans = vec![Span::raw("   ")];
                                spans.extend(line.spans.iter().cloned());
                                let mut new_line = Line::from(spans);
                                new_line.style = line.style;
                                new_line.alignment = line.alignment;
                                lines.push(new_line);
                            }
                            true
                        })
                        .unwrap_or(false);

                    #[cfg(not(feature = "markdown"))]
                    let used_markdown = false;

                    if !used_markdown {
                        // Source of truth: stream.raw_text() when markdown is
                        // on (the stream owns the text); entry.text otherwise.
                        #[cfg(feature = "markdown")]
                        let source: &str = entry
                            .markdown
                            .as_ref()
                            .map(|s| s.raw_text())
                            .unwrap_or(entry.text.as_str());
                        #[cfg(not(feature = "markdown"))]
                        let source: &str = entry.text.as_str();

                        for text_line in source.lines() {
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

                TraceKind::Act { tool, args } => {
                    // Header line: timestamp + "🔧 ACT  {tool}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("🔧 ACT  {}", tool),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Args below if present
                    if !args.is_empty() {
                        for arg_line in args.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    arg_line.to_string(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                    // Entry text (e.g. additional description)
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

                TraceKind::Observe => {
                    // Header line: timestamp + "👁 OBSERVE"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "👁 OBSERVE",
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines indented
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

                TraceKind::Delegate { agent, task, status } => {
                    // Header line: timestamp + "→ DELEGATE to {agent}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("→ DELEGATE to {}", agent),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Task line
                    if !task.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                task.clone(),
                                Style::default().fg(Color::Cyan),
                            ),
                        ]));
                    }
                    // Status with spinner if active
                    if !status.is_empty() {
                        let is_active = status == "running" || status == "active" || status == "delegated";
                        let status_text = if is_active {
                            format!("   {} {}", spinner_frame, status)
                        } else {
                            format!("   {}", status)
                        };
                        lines.push(Line::from(vec![Span::styled(
                            status_text,
                            Style::default().fg(Color::Cyan),
                        )]));
                    }
                }

                TraceKind::UserMessage => {
                    // Header line: timestamp + "💬 YOU"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "💬 YOU",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines
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

                TraceKind::Permission {
                    description,
                    pending,
                    countdown,
                } => {
                    // Header line: timestamp + "⚠ PERMISSION: {description}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("⚠ PERMISSION: {}", description),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Key hints + optional countdown
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
                    // Body text if any
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

            // Blank separator between entries
            lines.push(Line::from(""));
        }

        lines
    }

    /// Flatten display lines into virtual rows (one row per unit).
    /// Backward-compatible wrapper: produces no `ImageRow` entries, since
    /// no heights are known. Fences fall back to single-row Text placeholders.
    #[cfg(feature = "markdown")]
    #[allow(dead_code)] // Consumed in Task 4/6; exercised now by test helper.
    pub(crate) fn build_virtual_rows(&self, effective_width: u16) -> Vec<VirtualRow> {
        let empty = std::collections::HashMap::new();
        self.build_virtual_rows_with_heights(effective_width, &empty)
    }

    /// Items-aware virtual row builder. Walks entries directly, and for
    /// `AgentMessage` entries iterates the markdown stream's items so
    /// `StreamItem::Fence(id)` can be expanded into N `ImageRow` entries
    /// when `heights[id] > 0`, or fall back to a single Text placeholder
    /// row otherwise.
    ///
    /// Duplicates some entry-kind rendering logic with `build_display_lines`;
    /// Task 6 consolidates.
    #[cfg(feature = "markdown")]
    pub(crate) fn build_virtual_rows_with_heights(
        &self,
        effective_width: u16,
        heights: &std::collections::HashMap<crate::components::mermaid::MermaidId, u16>,
    ) -> Vec<VirtualRow> {
        use crate::components::markdown_stream::StreamItem;

        let spinner_frame =
            SPINNER_FRAMES[(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];

        let mut rows: Vec<VirtualRow> = Vec::new();

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

        for entry in &self.entries {
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(Color::DarkGray),
            );

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

                    let items_rendered = entry
                        .markdown
                        .as_ref()
                        .filter(|stream| !stream.items().is_empty())
                        .map(|stream| {
                            for item in stream.items() {
                                match item {
                                    StreamItem::Text(text_lines) => {
                                        for line in text_lines {
                                            // Indent by 3 spaces to match
                                            // AgentMessage formatting.
                                            let mut spans = vec![Span::raw("   ")];
                                            spans.extend(line.spans.iter().cloned());
                                            let mut new_line = Line::from(spans);
                                            new_line.style = line.style;
                                            new_line.alignment = line.alignment;
                                            push_wrapped(&mut rows, new_line);
                                        }
                                    }
                                    StreamItem::Fence(id) => {
                                        let h = heights.get(id).copied().unwrap_or(0);
                                        if h > 0 {
                                            for r in 0..h {
                                                rows.push(VirtualRow::ImageRow {
                                                    id: *id,
                                                    row_within: r,
                                                    total_rows: h,
                                                });
                                            }
                                        } else {
                                            let placeholder = format!(
                                                "   [📊 mermaid #{} · press Alt-v to view]",
                                                id.0
                                            );
                                            let line = Line::from(Span::styled(
                                                placeholder,
                                                Style::default()
                                                    .fg(Color::Magenta)
                                                    .add_modifier(Modifier::BOLD),
                                            ));
                                            push_wrapped(&mut rows, line);
                                        }
                                    }
                                }
                            }
                            true
                        })
                        .unwrap_or(false);

                    if !items_rendered {
                        let source: &str = entry
                            .markdown
                            .as_ref()
                            .map(|s| s.raw_text())
                            .unwrap_or(entry.text.as_str());
                        for text_line in source.lines() {
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

                TraceKind::Act { tool, args } => {
                    push_wrapped(
                        &mut rows,
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("🔧 ACT  {}", tool),
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );
                    if !args.is_empty() {
                        for arg_line in args.lines() {
                            push_wrapped(
                                &mut rows,
                                Line::from(vec![
                                    Span::raw("   "),
                                    Span::styled(
                                        arg_line.to_string(),
                                        Style::default().fg(Color::Yellow),
                                    ),
                                ]),
                            );
                        }
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

                TraceKind::Observe => {
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

                TraceKind::Delegate { agent, task, status } => {
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
                                Span::styled(
                                    task.clone(),
                                    Style::default().fg(Color::Cyan),
                                ),
                            ]),
                        );
                    }
                    if !status.is_empty() {
                        let is_active = status == "running"
                            || status == "active"
                            || status == "delegated";
                        let status_text = if is_active {
                            format!("   {} {}", spinner_frame, status)
                        } else {
                            format!("   {}", status)
                        };
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![Span::styled(
                                status_text,
                                Style::default().fg(Color::Cyan),
                            )]),
                        );
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
                            format!(
                                "   [y]es [n]o [a]lways  (auto-deny in {}s)",
                                countdown
                            )
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

            // Blank separator between entries.
            push_wrapped(&mut rows, Line::from(""));
        }

        rows
    }

    /// Render the full ReAct trace into the given frame area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let block = Block::default()
            .title(" Session ")
            .title_bottom(following_indicator)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let spinner_frame = SPINNER_FRAMES
            [(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];

        let lines = self.build_display_lines(spinner_frame);

        // Pre-wrap every built Line to the inner width so the Paragraph
        // renders row-exact and scroll offsets are exact visual rows.
        let inner = block.inner(area);
        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        let wrapped: Vec<Line> = lines
            .into_iter()
            .flat_map(|l| wrap_line_to_width(&l, effective_width))
            .collect();

        let total_lines = wrapped.len();
        self.last_total_lines.set(total_lines);
        self.last_visible_height.set(visible_height);

        // Clamp or pin scroll offset.
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        // Paragraph renders each pre-wrapped Line as one visual row. No
        // `.wrap()` — we already sized every Line to `effective_width`.
        let paragraph = Paragraph::new(wrapped)
            .block(block)
            .scroll((offset as u16, 0));

        frame.render_widget(paragraph, area);

        // Scrollbar: proportional thumb via viewport_content_length.
        if total_lines > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(offset)
                .viewport_content_length(visible_height);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl Default for ReactTrace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "markdown"))]
impl ReactTrace {
    pub(crate) fn total_virtual_rows_for_test(&self, effective_width: u16) -> usize {
        self.build_virtual_rows(effective_width).len()
    }

    pub(crate) fn build_virtual_rows_with_heights_for_test(
        &self,
        effective_width: u16,
        heights: &std::collections::HashMap<crate::components::mermaid::MermaidId, u16>,
    ) -> Vec<VirtualRow> {
        self.build_virtual_rows_with_heights(effective_width, heights)
    }
}

#[cfg(test)]
impl ReactTrace {
    /// Test-only helper: mimics the line-building portion of `render` but
    /// returns each line as a joined `String` of span text, so tests can
    /// assert on visible content without needing a real `Frame`.
    pub(crate) fn render_lines_for_test(&self, _width: u16) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        for entry in &self.entries {
            match &entry.kind {
                TraceKind::Think => {
                    lines.push(format!("{} 🧠 THINK", entry.timestamp));
                    for text_line in entry.text.lines() {
                        lines.push(format!("   {}", text_line));
                    }
                }

                TraceKind::AgentMessage { agent } => {
                    lines.push(format!("{} ✉ {}", entry.timestamp, agent));

                    #[cfg(feature = "markdown")]
                    let used_markdown = entry
                        .markdown
                        .as_ref()
                        .filter(|stream| !stream.items().is_empty())
                        .map(|stream| {
                            for line in stream.lines() {
                                let joined: String =
                                    line.spans.iter().map(|s| s.content.as_ref()).collect();
                                lines.push(format!("   {}", joined));
                            }
                            true
                        })
                        .unwrap_or(false);

                    #[cfg(not(feature = "markdown"))]
                    let used_markdown = false;

                    if !used_markdown {
                        #[cfg(feature = "markdown")]
                        let source: &str = entry
                            .markdown
                            .as_ref()
                            .map(|s| s.raw_text())
                            .unwrap_or(entry.text.as_str());
                        #[cfg(not(feature = "markdown"))]
                        let source: &str = entry.text.as_str();

                        for text_line in source.lines() {
                            lines.push(format!("   {}", text_line));
                        }
                    }
                }

                TraceKind::Act { tool, args } => {
                    lines.push(format!("{} 🔧 ACT  {}", entry.timestamp, tool));
                    for arg_line in args.lines() {
                        lines.push(format!("   {}", arg_line));
                    }
                    for text_line in entry.text.lines() {
                        lines.push(format!("   {}", text_line));
                    }
                }

                TraceKind::Observe => {
                    lines.push(format!("{} 👁 OBSERVE", entry.timestamp));
                    for text_line in entry.text.lines() {
                        lines.push(format!("   {}", text_line));
                    }
                }

                TraceKind::Delegate { agent, task, status } => {
                    lines.push(format!("{} → DELEGATE to {}", entry.timestamp, agent));
                    if !task.is_empty() {
                        lines.push(format!("   {}", task));
                    }
                    if !status.is_empty() {
                        lines.push(format!("   {}", status));
                    }
                }

                TraceKind::UserMessage => {
                    lines.push(format!("{} 💬 YOU", entry.timestamp));
                    for text_line in entry.text.lines() {
                        lines.push(format!("   {}", text_line));
                    }
                }

                TraceKind::Permission { description, pending, countdown } => {
                    lines.push(format!("{} ⚠ PERMISSION: {}", entry.timestamp, description));
                    if *pending {
                        if *countdown > 0 {
                            lines.push(format!(
                                "   [y]es [n]o [a]lways  (auto-deny in {}s)",
                                countdown
                            ));
                        } else {
                            lines.push("   [y]es [n]o [a]lways".to_string());
                        }
                    }
                    for text_line in entry.text.lines() {
                        lines.push(format!("   {}", text_line));
                    }
                }
            }

            lines.push(String::new());
        }

        lines
    }
}

#[cfg(all(test, feature = "markdown"))]
mod markdown_integration_tests {
    use super::*;

    /// Regression: when the first AgentMessageChunk arrives, the render
    /// frame that immediately follows must show the text body, not a
    /// blank region waiting for the 50ms debounce to fire.
    #[test]
    fn first_chunk_renders_body_before_debounce_flush() {
        let mut trace = ReactTrace::new();
        trace.append_message("Hello, world!", "claude", "10:00:00".to_string());

        let rendered = trace.render_lines_for_test(60);
        let joined = rendered.join("\n");
        assert!(
            joined.contains("Hello, world!"),
            "expected first chunk text to be visible in render output, got:\n{joined}"
        );
    }

    /// Once the debounce flushes and styled lines are cached, render uses
    /// them (not the plain-text fallback). Assert by checking that the
    /// content is still present after flush.
    #[test]
    fn post_flush_rendered_lines_still_show_text() {
        use crate::components::markdown_stream::StateLookup;
        let mut trace = ReactTrace::new();
        trace.append_message("# Heading\n\nBody text", "claude", "10:00:00".to_string());

        // Force a flush via drain_fence_dispatches with empty state.
        let states = StateLookup::empty();
        let _ = trace.drain_fence_dispatches(&states);

        let rendered = trace.render_lines_for_test(60);
        let joined = rendered.join("\n");
        assert!(joined.contains("Heading"), "expected heading text after flush: {joined}");
        assert!(joined.contains("Body text"), "expected body text after flush: {joined}");
    }

    #[test]
    fn items_path_renders_same_text_as_lines_path() {
        let mut trace = ReactTrace::new();
        trace.append_message("# Heading\n\nBody", "claude", "10:00".to_string());
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let rendered = trace.render_lines_for_test(60);
        let joined = rendered.join("\n");
        assert!(joined.contains("Heading"), "expected heading: {joined}");
        assert!(joined.contains("Body"), "expected body: {joined}");
    }
}

#[cfg(all(test, feature = "markdown"))]
mod virtual_row_tests {
    use super::*;

    #[test]
    fn virtual_rows_text_only_match_line_count() {
        let mut trace = ReactTrace::new();
        trace.append_message("Line 1\nLine 2\nLine 3", "claude", "10:00".to_string());
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let total = trace.total_virtual_rows_for_test(60);
        // Header (1) + 3 body lines + blank separator (1) = 5
        assert_eq!(total, 5, "unexpected virtual row count: {total}");
    }

    #[test]
    fn fence_with_ready_state_expands_to_image_rows() {
        let mut trace = ReactTrace::new();
        trace.append_message(
            "Before\n\n```mermaid\ngraph\nA-->B\n```\n\nAfter\n",
            "claude",
            "10:00".to_string(),
        );
        use crate::components::markdown_stream::StateLookup;
        // Force flush so the stream's `items()` is populated (bypasses
        // the 50 ms debounce that `drain_fence_dispatches` observes).
        let _ = trace.force_flush_all(&StateLookup::empty());
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        use std::collections::HashMap;
        let mut heights: HashMap<crate::components::mermaid::MermaidId, u16> = HashMap::new();
        heights.insert(crate::components::mermaid::MermaidId(0), 12);

        let rows = trace.build_virtual_rows_with_heights_for_test(60, &heights);

        let image_rows: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                VirtualRow::ImageRow { id, row_within, total_rows } => {
                    Some((*id, *row_within, *total_rows))
                }
                _ => None,
            })
            .collect();

        assert_eq!(image_rows.len(), 12, "expected 12 image rows; got {image_rows:?}");
        assert_eq!(image_rows[0].2, 12, "total_rows");
        assert_eq!(image_rows[0].1, 0, "first row_within");
        assert_eq!(image_rows[11].1, 11, "last row_within");
    }

    #[test]
    fn fence_without_height_emits_single_placeholder_row() {
        let mut trace = ReactTrace::new();
        trace.append_message(
            "Before\n\n```mermaid\ngraph\n```\n\nAfter\n",
            "claude",
            "10:00".to_string(),
        );
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.force_flush_all(&StateLookup::empty());
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let empty = std::collections::HashMap::new();
        let rows = trace.build_virtual_rows_with_heights_for_test(60, &empty);

        let image_rows = rows
            .iter()
            .filter(|r| matches!(r, VirtualRow::ImageRow { .. }))
            .count();
        assert_eq!(image_rows, 0, "should fall back to Text placeholder");
    }
}
