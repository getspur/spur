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
    Delegate {
        agent: String,
        task: String,
        status: String,
        /// UUID from spur-mcp; matches the brain's delegate_to_worker call.
        /// Some once `DelegationRequested` is consumed.
        request_id: Option<String>,
        /// The spawned executor; Some after `DelegationDispatched` arrives.
        /// Used by render path to embed an inline executor card.
        executor_id: Option<String>,
    },
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

#[cfg(feature = "markdown")]
#[derive(Debug, Clone)]
pub(crate) enum VirtualRow {
    Text(Line<'static>),
    ImageRow {
        id: crate::components::mermaid::MermaidId,
        row_within: u16,
        total_rows: u16,
    },
}

#[cfg(feature = "markdown")]
pub struct RenderContext<'a> {
    pub mermaid_registry: &'a std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    pub picker: Option<&'a ratatui_image::picker::Picker>,
}

#[cfg(feature = "markdown")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment {
    Text {
        start: usize,
        len: usize,
    },
    Image {
        id: crate::components::mermaid::MermaidId,
        total_rows: u16,
        first_row_within: u16,
        run_len: u16,
    },
}

/// Group contiguous virtual rows into render batches. Only rows in
/// `[start_idx, end_idx)` are considered.
#[cfg(feature = "markdown")]
pub(crate) fn segment_visible_rows(
    rows: &[VirtualRow],
    start_idx: usize,
    end_idx: usize,
) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut i = start_idx;
    while i < end_idx {
        match &rows[i] {
            VirtualRow::Text(_) => {
                let start = i;
                while i < end_idx && matches!(rows[i], VirtualRow::Text(_)) {
                    i += 1;
                }
                out.push(Segment::Text { start, len: i - start });
            }
            VirtualRow::ImageRow { id, row_within, total_rows } => {
                let run_id = *id;
                let run_total = *total_rows;
                let first_within = *row_within;
                let start = i;
                while i < end_idx {
                    if let VirtualRow::ImageRow { id: id2, .. } = &rows[i] {
                        if *id2 == run_id {
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }
                out.push(Segment::Image {
                    id: run_id,
                    total_rows: run_total,
                    first_row_within: first_within,
                    run_len: (i - start) as u16,
                });
            }
        }
    }
    out
}

/// Row count for rendering an image inline at `pane_width_cols` with aspect
/// ratio preserved. Clamped to `[6, 60]` rows — short enough not to swamp
/// the pane, tall enough for realistic diagrams to render without squishing.
///
/// Without aspect-correct sizing, `ratatui_image::Resize::Fit` scales by
/// the tighter of (width ratio, height ratio): a tall image in a short
/// Rect letterboxes narrow and shrinks text below legibility.
#[cfg(feature = "markdown")]
pub(crate) fn compute_inline_height_rows(
    image: &image::DynamicImage,
    pane_width_cols: u16,
    picker: Option<&ratatui_image::picker::Picker>,
) -> u16 {
    let (cell_w_px, cell_h_px) = picker
        .map(|p| {
            let (w, h) = p.font_size();
            (w.max(1) as u32, h.max(1) as u32)
        })
        .unwrap_or((8, 16));

    let pane_width_px = (pane_width_cols as u32).saturating_mul(cell_w_px);
    if pane_width_px == 0 || image.width() == 0 {
        return 6;
    }
    // display_h_px = image_h × (pane_w_px / image_w); rows = display_h_px / cell_h.
    let scaled_h_px = ((image.height() as u64) * (pane_width_px as u64))
        .div_ceil(image.width() as u64) as u32;
    let rows = scaled_h_px.div_ceil(cell_h_px);
    rows.clamp(6, 60) as u16
}

#[cfg(feature = "markdown")]
fn compute_fence_states(
    ctx: &RenderContext<'_>,
    pane_width_cols: u16,
) -> std::collections::HashMap<
    crate::components::mermaid::MermaidId,
    crate::components::mermaid::FenceRender,
> {
    use crate::components::mermaid::{FenceRender, MermaidState};
    let mut out = std::collections::HashMap::new();
    for (id, state) in ctx.mermaid_registry.iter() {
        let r = match state {
            MermaidState::Ready { image, .. } => FenceRender::Ready(
                compute_inline_height_rows(image.as_ref(), pane_width_cols, ctx.picker),
            ),
            MermaidState::Pending { .. } | MermaidState::Rendering => FenceRender::Pending,
            MermaidState::Error { .. } => FenceRender::Error,
        };
        out.insert(*id, r);
    }
    out
}

/// Render the inline image for a `Ready` diagram into `rect`. Returns true
/// if the image widget was rendered; false if the caller should fall back
/// to a text placeholder.
#[cfg(feature = "markdown")]
fn render_inline_image(
    frame: &mut Frame,
    rect: Rect,
    id: crate::components::mermaid::MermaidId,
    ctx: &RenderContext<'_>,
) -> bool {
    use crate::components::mermaid::MermaidState;
    use ratatui_image::{Resize, StatefulImage};

    let Some(MermaidState::Ready { image, inline_protocol }) =
        ctx.mermaid_registry.get(&id)
    else {
        return false;
    };
    let Some(picker) = ctx.picker else {
        return false;
    };

    let mut slot = inline_protocol.borrow_mut();
    if slot.is_none() {
        // Unavoidable pixel copy: ratatui_image takes DynamicImage by value
        // to build the protocol. Arc prevents repeated deep-copies elsewhere.
        *slot = Some(picker.new_resize_protocol((**image).clone()));
    }
    let Some(proto) = slot.as_mut() else {
        return false;
    };
    let widget = StatefulImage::default().resize(Resize::Fit(None));
    frame.render_stateful_widget(widget, rect, proto);
    true
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
    /// Whether mermaid rendering is available. Forwarded to newly-created
    /// `MarkdownStream` instances so ```mermaid fences render as ordinary
    /// code blocks when the terminal lacks image-protocol support.
    mermaid_enabled: bool,
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
            mermaid_enabled: true,
        }
    }

    /// Set whether ```mermaid fences should be rendered as images. Called
    /// from the session-view layer once the terminal's image-protocol
    /// capability is known. Affects streams created after this call.
    pub fn set_mermaid_enabled(&mut self, enabled: bool) {
        self.mermaid_enabled = enabled;
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

    /// Append text to the most recent AgentMessage entry for the same agent,
    /// or create a new one. Walks backwards up to a small bounded window
    /// skipping non-message entries (tool calls, observations, etc.) so that
    /// interleaved tool calls don't split one logical agent message into
    /// multiple fragments (S1.b fix for H2).
    pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String) {
        // Walk backwards up to a small bounded window looking for the most
        // recent AgentMessage for THIS agent, skipping non-message entries
        // (tool calls, observations, etc.). Stop at a user turn or a
        // different agent's message — don't merge across those boundaries.
        const WALKBACK_LIMIT: usize = 10;
        let mut target_idx: Option<usize> = None;
        for (offset, entry) in self.entries.iter().rev().take(WALKBACK_LIMIT).enumerate() {
            match &entry.kind {
                TraceKind::AgentMessage { agent: entry_agent } if entry_agent == agent => {
                    target_idx = Some(self.entries.len() - 1 - offset);
                    break;
                }
                // Don't walk past user turns or other agents' messages.
                TraceKind::UserMessage => break,
                TraceKind::AgentMessage { .. } => break, // different agent
                _ => continue,                            // tool call / think / etc.
            }
        }

        #[cfg(feature = "markdown")]
        {
            if let Some(idx) = target_idx {
                if let Some(entry) = self.entries.get_mut(idx) {
                    if let Some(stream) = entry.markdown.as_mut() {
                        stream.append(text);
                    }
                    if self.is_following {
                        self.scroll_to_bottom();
                    }
                    return;
                }
            }
            let mut stream =
                super::markdown_stream::MarkdownStream::new_with_mermaid(self.mermaid_enabled);
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
            if let Some(idx) = target_idx {
                if let Some(entry) = self.entries.get_mut(idx) {
                    entry.text.push_str(text);
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
    fn build_display_lines(&self, spinner_frame: &str, lineage: Option<&spur_core::lineage::projection::ExecutorLineage>) -> Vec<Line<'static>> {
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

                TraceKind::Delegate { agent, task, status, request_id: _, executor_id } => {
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
                    // After the bare status lines, embed the live executor card
                    // if we can correlate to a lineage node.
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

    /// Items-aware virtual row builder. Walks entries directly, and for
    /// `AgentMessage` entries iterates the markdown stream's items so
    /// `StreamItem::Fence(id)` can be expanded into N `ImageRow` entries
    /// when the fence is `Ready(h)`, or fall back to a state-aware single-row
    /// placeholder (⏳ Pending, ⚠ Error, 📊 default) otherwise.
    ///
    /// Duplicates some entry-kind rendering logic with `build_display_lines`;
    /// future work can consolidate.
    #[cfg(feature = "markdown")]
    pub(crate) fn build_virtual_rows(
        &self,
        effective_width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
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
                                        use crate::components::mermaid::FenceRender;
                                        match states.get(id).copied() {
                                            Some(FenceRender::Ready(h)) if h > 0 => {
                                                for r in 0..h {
                                                    rows.push(VirtualRow::ImageRow {
                                                        id: *id,
                                                        row_within: r,
                                                        total_rows: h,
                                                    });
                                                }
                                            }
                                            other => {
                                                // Ready(0) is effectively still-rendering
                                                // from the inline-render POV; fold into
                                                // Pending. None → not yet in registry.
                                                let render = match other {
                                                    Some(FenceRender::Error) => FenceRender::Error,
                                                    _ => FenceRender::Pending,
                                                };
                                                // Shared helper produces an un-indented
                                                // Line; re-indent to match AgentMessage body.
                                                let placeholder =
                                                    crate::components::mermaid::fence_placeholder_line(
                                                        *id, render,
                                                    );
                                                let mut spans = vec![Span::raw("   ")];
                                                spans.extend(placeholder.spans.iter().cloned());
                                                let mut line = Line::from(spans);
                                                line.style = placeholder.style;
                                                line.alignment = placeholder.alignment;
                                                push_wrapped(&mut rows, line);
                                            }
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

                TraceKind::Delegate { agent, task, status, request_id: _, executor_id } => {
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
                    // Splice inline executor card if we can correlate to a lineage node.
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
    ///
    /// Non-markdown path. For markdown-enabled sessions, callers should use
    /// `render_with_ctx` which supports inline image segments.
    pub fn render(&self, frame: &mut Frame, area: Rect, lineage: Option<&spur_core::lineage::projection::ExecutorLineage>) {
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

        let lines = self.build_display_lines(spinner_frame, lineage);

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

    /// Render the trace with markdown + inline mermaid support.
    ///
    /// Walks virtual rows, batching contiguous text rows into `Paragraph`
    /// Rects and contiguous `ImageRow` runs per diagram into `StatefulImage`
    /// Rects. Partial-image runs (scrolled so the diagram is cropped) render
    /// as a single-row placeholder instead — the v1 graceful-clip policy.
    #[cfg(feature = "markdown")]
    pub fn render_with_ctx(
        &self,
        frame: &mut Frame,
        area: Rect,
        ctx: &RenderContext<'_>,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) {
        use crate::components::mermaid::MermaidState;

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

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        let states = compute_fence_states(ctx, effective_width);
        let rows = self.build_virtual_rows(effective_width, &states, lineage);

        let total = rows.len();
        self.last_total_lines.set(total);
        self.last_visible_height.set(visible_height);

        let max_offset = total.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        let visible_end = (offset + visible_height).min(total);
        let segments = segment_visible_rows(&rows, offset, visible_end);

        // Walk segments and render into sub-Rects of `inner`.
        let mut y: u16 = inner.y;
        for seg in segments {
            match seg {
                Segment::Text { start, len } => {
                    let height = len as u16;
                    let rect = Rect { x: inner.x, y, width: inner.width, height };
                    let lines: Vec<Line<'static>> = rows[start..start + len]
                        .iter()
                        .map(|r| match r {
                            VirtualRow::Text(l) => l.clone(),
                            // Should not happen — segmenter groups by kind.
                            VirtualRow::ImageRow { .. } => Line::from(""),
                        })
                        .collect();
                    frame.render_widget(Paragraph::new(lines), rect);
                    y += height;
                }
                Segment::Image { id, total_rows, first_row_within, run_len } => {
                    let rect = Rect { x: inner.x, y, width: inner.width, height: run_len };
                    let fully_visible =
                        first_row_within == 0 && run_len == total_rows;

                    let drew_image = if fully_visible {
                        render_inline_image(frame, rect, id, ctx)
                    } else {
                        false
                    };

                    if !drew_image {
                        let msg = if !fully_visible {
                            format!("   [📊 mermaid #{} · scroll to align · Alt-v to zoom]", id.0)
                        } else if !matches!(
                            ctx.mermaid_registry.get(&id),
                            Some(MermaidState::Ready { .. })
                        ) {
                            format!("   [📊 mermaid #{} · not ready]", id.0)
                        } else {
                            format!("   [📊 mermaid #{} · no graphics protocol]", id.0)
                        };
                        let line = Line::from(Span::styled(
                            msg,
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ));
                        frame.render_widget(Paragraph::new(vec![line]), rect);
                    }
                    y += run_len;
                }
            }
        }

        // Scrollbar — same math as non-markdown path.
        if total > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total)
                .position(offset)
                .viewport_content_length(visible_height);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Expose the trace entries as a slice for testing and inspection.
    pub fn entries(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Test-only accessor (available in normal builds so integration tests
    /// can reach it transitively via `SessionDetailView::trace_snapshot_for_test`).
    #[doc(hidden)]
    pub(crate) fn entries_for_test(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Return the text of the most recent entry, or `None` if the trace is
    /// empty. Used in tests to assert on system-note content.
    #[cfg(test)]
    pub fn last_text(&self) -> Option<String> {
        self.entries.last().map(|e| e.text.clone())
    }

    /// Locate the most recent `Delegate` entry whose `request_id` matches
    /// the given UUID and attach the `executor_id`. No-op if not found
    /// (event arrived for an entry not in this trace, or out of order).
    pub fn attach_executor_id(&mut self, request_id: &str, executor_id: &str) {
        for entry in self.entries.iter_mut().rev() {
            if let TraceKind::Delegate {
                request_id: Some(rid),
                executor_id: slot @ None,
                ..
            } = &mut entry.kind
            {
                if rid == request_id {
                    *slot = Some(executor_id.to_string());
                    return;
                }
            }
        }
        tracing::debug!(
            request_id = %request_id,
            executor_id = %executor_id,
            "DelegationDispatched arrived but no matching Delegate entry"
        );
    }
}

impl Default for ReactTrace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "markdown"))]
impl ReactTrace {
    /// Test helper: compute the render segmentation without a real frame.
    /// Mirrors what `render_with_ctx` computes internally.
    pub(crate) fn render_plan_for_test(
        &self,
        effective_width: u16,
        visible_height: usize,
        offset: usize,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
    ) -> Vec<Segment> {
        let rows = self.build_virtual_rows(effective_width, states, None);
        let end = (offset + visible_height).min(rows.len());
        segment_visible_rows(&rows, offset, end)
    }
}

#[cfg(test)]
impl ReactTrace {
    /// Test-only helper: mutable access to the entry list so tests can
    /// inspect per-entry `MarkdownStream` state.
    pub(crate) fn entries_mut_for_test(&mut self) -> &mut [TraceEntry] {
        &mut self.entries
    }

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

                TraceKind::Delegate { agent, task, status, request_id: _, executor_id: _ } => {
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

    #[test]
    fn text_mode_agent_message_stream_produces_no_fence_items() {
        use crate::components::markdown_stream::{StateLookup, StreamItem};
        let mut trace = ReactTrace::new();
        trace.set_mermaid_enabled(false);
        trace.append_message(
            "Here's a diagram:\n\n```mermaid\nflowchart LR\nA-->B\n```\n",
            "claude",
            "10:00".to_string(),
        );
        let states = StateLookup::empty();
        let _ = trace.force_flush_all(&states);

        let entries = trace.entries_mut_for_test();
        assert!(!entries.is_empty(), "expected at least one entry");
        for entry in entries {
            if let Some(stream) = entry.markdown.as_mut() {
                let has_fence = stream
                    .items()
                    .iter()
                    .any(|it| matches!(it, StreamItem::Fence(_)));
                assert!(
                    !has_fence,
                    "text mode must not produce Fence items: {:?}",
                    stream.items()
                );
            }
        }
    }
}

#[cfg(all(test, feature = "markdown"))]
mod virtual_row_tests {
    use super::*;
    use crate::components::mermaid::FenceRender;

    #[test]
    fn virtual_rows_text_only_match_line_count() {
        let mut trace = ReactTrace::new();
        trace.append_message("Line 1\nLine 2\nLine 3", "claude", "10:00".to_string());
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let total = trace
            .build_virtual_rows(60, &std::collections::HashMap::new(), None)
            .len();
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
        let mut states: HashMap<crate::components::mermaid::MermaidId, FenceRender> =
            HashMap::new();
        states.insert(crate::components::mermaid::MermaidId(0), FenceRender::Ready(12));

        let rows = trace.build_virtual_rows(60, &states, None);

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
        let rows = trace.build_virtual_rows(60, &empty, None);

        let image_rows = rows
            .iter()
            .filter(|r| matches!(r, VirtualRow::ImageRow { .. }))
            .count();
        assert_eq!(image_rows, 0, "should fall back to Text placeholder");
    }

    #[test]
    fn render_plan_groups_contiguous_text_and_images() {
        let mut trace = ReactTrace::new();
        trace.append_message(
            "Before text line\n\n```mermaid\ngraph\n```\n\nAfter text line\n",
            "claude",
            "10:00".to_string(),
        );
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.force_flush_all(&StateLookup::empty());

        let mut states = std::collections::HashMap::new();
        states.insert(crate::components::mermaid::MermaidId(0), FenceRender::Ready(8));

        let segments = trace.render_plan_for_test(80, 40, 0, &states);

        let image_segs: Vec<_> = segments
            .iter()
            .filter(|s| matches!(s, Segment::Image { .. }))
            .collect();
        assert_eq!(image_segs.len(), 1, "expected exactly one image segment: {segments:?}");
        if let Segment::Image { total_rows, first_row_within, run_len, .. } = image_segs[0] {
            assert_eq!(*total_rows, 8);
            assert_eq!(*first_row_within, 0);
            assert_eq!(*run_len, 8);
        }

        let text_count = segments
            .iter()
            .filter(|s| matches!(s, Segment::Text { .. }))
            .count();
        assert!(text_count >= 2, "expected >=2 text segments: {segments:?}");
    }

    #[test]
    fn render_plan_partial_image_marks_partial_visibility() {
        let mut trace = ReactTrace::new();
        trace.append_message(
            "L1\nL2\nL3\nL4\n\n```mermaid\ngraph\n```\n",
            "claude",
            "10:00".to_string(),
        );
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.force_flush_all(&StateLookup::empty());

        let mut states = std::collections::HashMap::new();
        states.insert(crate::components::mermaid::MermaidId(0), FenceRender::Ready(10));

        // Iterate offsets; some must produce a partial image (run_len < total).
        let mut saw_partial = false;
        for offset in 0..20 {
            let segs = trace.render_plan_for_test(80, 6, offset, &states);
            if segs.iter().any(|s| matches!(
                s,
                Segment::Image { total_rows, run_len, .. } if run_len < total_rows
            )) {
                saw_partial = true;
                break;
            }
        }
        assert!(saw_partial, "expected some offset to produce partial image segment");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H2 regression: when a non-AgentMessage entry (e.g. a tool call
    /// → `TraceKind::Act`) lands between two `AgentMessageChunk` events
    /// from the same agent, `append_message` today creates a NEW
    /// `AgentMessage` entry instead of continuing the previous one.
    /// The user sees one logical assistant response split into fragments.
    ///
    /// Task 4 implements the walkback fix; this test must FAIL until then.
    #[test]
    fn append_message_continues_existing_after_tool_call_interleave() {
        let mut trace = ReactTrace::new();
        trace.append_message("first chunk. ", "claude", "10:00:01".to_string());
        // Simulate a tool call landing between chunks (from session/update:
        // ToolCall or ToolCallUpdate variants). ReactTrace tracks tool calls
        // as TraceKind::Act.
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "read_file".to_string(),
                args: String::new(),
            },
            text: "read_file(path=...)".to_string(),
            timestamp: "10:00:02".to_string(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        trace.append_message("second chunk.", "claude", "10:00:03".to_string());

        // Count AgentMessage entries with agent="claude" — must be ONE merged.
        let entries = trace.entries_for_test();
        let agent_message_count = entries
            .iter()
            .filter(|e| {
                matches!(&e.kind, TraceKind::AgentMessage { agent } if agent == "claude")
            })
            .count();
        assert_eq!(
            agent_message_count, 1,
            "interleaved tool call split AgentMessage — H2 regressed"
        );

        // The single AgentMessage should contain both chunks. Under the
        // `markdown` feature the raw text lives inside the stream; fall
        // back to the stream's `raw_text()` when `entry.text` is empty.
        let msg_text = entries
            .iter()
            .find_map(|e| match &e.kind {
                TraceKind::AgentMessage { .. } => {
                    #[cfg(feature = "markdown")]
                    {
                        if !e.text.is_empty() {
                            Some(e.text.clone())
                        } else {
                            e.markdown.as_ref().map(|s| s.raw_text().to_string())
                        }
                    }
                    #[cfg(not(feature = "markdown"))]
                    {
                        Some(e.text.clone())
                    }
                }
                _ => None,
            })
            .expect("expected AgentMessage entry");
        assert!(
            msg_text.contains("first chunk"),
            "missing first chunk in merged message: {msg_text:?}"
        );
        assert!(
            msg_text.contains("second chunk"),
            "missing second chunk in merged message: {msg_text:?}"
        );
    }
}
