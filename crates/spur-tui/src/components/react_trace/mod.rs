mod builder;
mod render;
mod types;

#[cfg(feature = "markdown")]
pub use types::RenderContext;
#[cfg(all(test, feature = "markdown"))]
pub(crate) use types::{Segment, VirtualRow};
pub use types::{TraceEntry, TraceKind};

use spur_acp::{
    adapter::{mode_badge, ToolInputDisplay},
    AgentKind,
};

use ratatui::style::Color;

use super::trace_format::{
    family_glyph, input_display_lines, input_summary, observe_compact, observe_payload_lines,
    observe_verb, outcome_glyph,
};
use super::MAX_LOG_ENTRIES;

/// Spinner frames for delegation animation.
pub(super) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct ReactTrace {
    pub(super) entries: Vec<TraceEntry>,
    pub(super) scroll_offset: usize,
    pub(super) is_following: bool,
    pub(super) tick_counter: u8,
    /// Cached total rendered lines from last render.
    pub(super) last_total_lines: usize,
    /// Cached visible height from last render.
    pub(super) last_visible_height: usize,
    /// Whether mermaid rendering is available.
    pub(super) mermaid_enabled: bool,
    /// Which agent brain backs this session; drives pane title + accent color.
    pub(super) agent_kind: AgentKind,
    /// Current session mode, if known (e.g. "plan", "acceptEdits").
    pub(super) current_mode: Option<String>,
    /// When true (default), Observe entries show a truncated preview.
    pub(super) observe_collapsed: bool,
    /// Generation counter bumped on every content mutation.
    pub(super) generation: u64,
    /// Index of the first entry needing row rebuild.
    pub(super) dirty_from: Option<usize>,
    /// Cached pre-wrapped display lines.
    #[cfg(not(feature = "markdown"))]
    pub(super) line_cache: Option<render::LineCacheEntry>,
    /// Cached virtual rows for the markdown render path.
    #[cfg(feature = "markdown")]
    pub(super) line_cache: Option<render::VirtualRowCacheEntry>,
}

impl ReactTrace {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            is_following: true,
            tick_counter: 0,
            last_total_lines: 0,
            last_visible_height: 20,
            mermaid_enabled: true,
            agent_kind: AgentKind::Generic,
            current_mode: None,
            observe_collapsed: true,
            generation: 0,
            dirty_from: None,
            line_cache: None,
        }
    }

    /// Create a `ReactTrace` with an explicit `AgentKind` for title + accent color.
    pub fn with_kind(kind: AgentKind) -> Self {
        Self {
            agent_kind: kind,
            ..Self::new()
        }
    }

    /// Store the current session mode id (e.g. "plan", "acceptEdits").
    pub fn set_mode(&mut self, mode: Option<String>) {
        self.current_mode = mode;
        self.invalidate_cache();
    }

    /// Toggle the collapsed state for Observe (tool-result) entries.
    pub fn toggle_observe_collapsed(&mut self) -> bool {
        self.observe_collapsed = !self.observe_collapsed;
        self.invalidate_cache();
        self.observe_collapsed
    }

    /// Whether observe entries are currently collapsed.
    pub fn observe_collapsed(&self) -> bool {
        self.observe_collapsed
    }

    /// Build the styled pane title + accent color from `agent_kind` and
    /// optional mode badge.
    pub(super) fn pane_title_and_color(&self) -> (String, Color) {
        let (base_title, accent) = match self.agent_kind {
            AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => {
                (" Session · claude ", Color::Magenta)
            }
            AgentKind::CodexAcp => (" Session · codex ", Color::Yellow),
            AgentKind::Kiro => (" Session · kiro ", Color::Cyan),
            AgentKind::Generic => (" Session ", Color::DarkGray),
        };
        let mut title = if let Some(mode_id) = &self.current_mode {
            if let Some(badge) = mode_badge(mode_id, self.agent_kind) {
                format!("{}· {} ", base_title, badge.short)
            } else {
                base_title.to_string()
            }
        } else {
            base_title.to_string()
        };
        if !self.observe_collapsed {
            title.push_str("· ⊞ expanded ");
        }
        (title, accent)
    }

    /// Set whether ```mermaid fences should be rendered as images.
    pub fn set_mermaid_enabled(&mut self, enabled: bool) {
        self.mermaid_enabled = enabled;
        self.invalidate_cache();
    }

    /// Bump the generation counter, which causes the next `render()` call
    /// to rebuild the line cache.
    fn invalidate_cache(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.dirty_from = Some(0);
    }

    /// Mark entries from `idx` onward as needing row rebuild.
    fn mark_dirty_from(&mut self, idx: usize) {
        self.generation = self.generation.wrapping_add(1);
        let prev = self.dirty_from;
        self.dirty_from = Some(prev.map_or(idx, |d| d.min(idx)));
    }

    /// Return a short kind name for the most recent entry, or `None` if empty.
    pub fn last_entry_kind_name(&self) -> Option<&'static str> {
        self.entries.last().map(|e| match &e.kind {
            TraceKind::Think => "think",
            TraceKind::AgentMessage { .. } => "agent_message",
            TraceKind::Act { .. } => "act",
            TraceKind::Observe { .. } => "observe",
            TraceKind::Delegate { .. } => "delegate",
            TraceKind::UserMessage => "user_message",
            TraceKind::Permission { .. } => "permission",
        })
    }

    /// Append text to the most recent THINK entry, or create a new one.
    pub fn append_think(&mut self, text: &str, timestamp: String) {
        if let Some(last) = self.entries.last_mut() {
            if matches!(last.kind, TraceKind::Think) {
                if !last.text.is_empty() {
                    last.text.push_str(text);
                } else {
                    last.text = text.to_string();
                }
                self.mark_dirty_from(self.entries.len() - 1);
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
        self.push(TraceEntry {
            kind: TraceKind::Think,
            text: text.to_string(),
            timestamp,
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Append text to the most recent AgentMessage entry for the same agent,
    /// or create a new one.
    pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String) {
        let target_idx = match self.entries.last() {
            Some(entry) => match &entry.kind {
                TraceKind::AgentMessage { agent: a } if a == agent => Some(self.entries.len() - 1),
                _ => None,
            },
            None => None,
        };

        #[cfg(feature = "markdown")]
        {
            if let Some(idx) = target_idx {
                if let Some(entry) = self.entries.get_mut(idx) {
                    if let Some(stream) = entry.markdown.as_mut() {
                        stream.append(text);
                    }
                    self.mark_dirty_from(idx);
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
                kind: TraceKind::AgentMessage {
                    agent: agent.to_string(),
                },
                text: String::new(),
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
                    self.mark_dirty_from(idx);
                    if self.is_following {
                        self.scroll_to_bottom();
                    }
                    return;
                }
            }
            self.push(TraceEntry {
                kind: TraceKind::AgentMessage {
                    agent: agent.to_string(),
                },
                text: text.to_string(),
                timestamp,
            });
        }
    }

    /// Append a streamed user-message chunk, coalescing into the tail entry
    /// iff that entry is `TraceKind::UserMessage`. Otherwise push a new
    /// `TraceKind::UserMessage` entry. Symmetric to `append_message` for
    /// agent chunks.
    pub fn append_user_message(&mut self, text: &str, timestamp: String) {
        let target_idx = match self.entries.last() {
            Some(entry) => match &entry.kind {
                TraceKind::UserMessage => Some(self.entries.len() - 1),
                _ => None,
            },
            None => None,
        };

        match target_idx {
            Some(idx) => {
                // Idempotency guard: if the tail already ends with this chunk,
                // the content was previously seeded (e.g. by push_user_message
                // from the HistoryEntry replay path). Skip the append to avoid
                // doubling.
                if self.entries[idx].text.ends_with(text) {
                    return;
                }
                self.entries[idx].text.push_str(text);
                self.mark_dirty_from(idx);
                if self.is_following {
                    self.scroll_to_bottom();
                }
            }
            None => {
                self.entries.push(TraceEntry {
                    kind: TraceKind::UserMessage,
                    text: text.to_string(),
                    timestamp,
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
                self.mark_dirty_from(self.entries.len() - 1);
                if self.is_following {
                    self.scroll_to_bottom();
                }
            }
        }
    }

    /// Push a new trace entry, evicting oldest if over capacity.
    pub fn push(&mut self, entry: TraceEntry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            self.scroll_offset = self.scroll_offset.saturating_sub(drain);
            self.invalidate_cache();
        } else {
            self.mark_dirty_from(self.entries.len().saturating_sub(2));
        }
        if self.is_following {
            self.scroll_to_bottom();
        }
    }

    fn max_offset(&self) -> usize {
        self.last_total_lines
            .saturating_sub(self.last_visible_height)
    }

    pub fn scroll_up(&mut self) {
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
        let jump = self.last_visible_height.saturating_sub(2).max(1);
        self.scroll_offset = self.scroll_offset.saturating_sub(jump);
        self.is_following = false;
    }

    pub fn page_down(&mut self) {
        let jump = self.last_visible_height.saturating_sub(2).max(1);
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

        let mut animation_idx: Option<usize> = None;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if let TraceKind::Permission {
                pending, countdown, ..
            } = &mut entry.kind
            {
                if *pending && *countdown > 0 {
                    *countdown = countdown.saturating_sub(1);
                    animation_idx = Some(animation_idx.map_or(i, |a| a.min(i)));
                }
            }
        }
        if animation_idx.is_none() {
            animation_idx = self.first_active_spinner();
        }
        if let Some(idx) = animation_idx {
            self.mark_dirty_from(idx);
        }
    }

    /// Returns the index of the first entry showing an animated spinner.
    fn first_active_spinner(&self) -> Option<usize> {
        let len = self.entries.len();
        for (i, entry) in self.entries.iter().enumerate() {
            if let TraceKind::Act { .. } = &entry.kind {
                if self.observe_collapsed {
                    let has_observe = i + 1 < len
                        && matches!(
                            &self.entries[i + 1].kind,
                            TraceKind::Observe { payload: Some(_) }
                        );
                    if !has_observe {
                        return Some(i);
                    }
                }
            }
        }
        None
    }

    /// Drain any mermaid fences detected during the last debounce window.
    #[cfg(feature = "markdown")]
    pub fn drain_fence_dispatches(
        &mut self,
        states: &super::markdown_stream::StateLookup<'_>,
    ) -> Vec<(usize, super::markdown_stream::FenceRef)> {
        let mut out = Vec::new();
        let mut first_flushed: Option<usize> = None;
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                let was_dirty = stream.is_dirty();
                for fence in stream.maybe_flush(states) {
                    out.push((idx, fence));
                }
                if was_dirty && !stream.is_dirty() {
                    first_flushed = Some(first_flushed.map_or(idx, |d| d.min(idx)));
                }
            }
        }
        if let Some(idx) = first_flushed {
            self.mark_dirty_from(idx);
        }
        out
    }

    #[cfg(not(feature = "markdown"))]
    pub fn drain_fence_dispatches(&mut self) -> Vec<(usize, ())> {
        Vec::new()
    }

    /// Force an immediate rebuild of every markdown stream.
    ///
    /// Used on TurnComplete — uses `flush_final` so trailing content
    /// (paragraphs / fence closes at EOF) gets committed and styled
    /// instead of rendering as plain tail.
    #[cfg(feature = "markdown")]
    pub fn force_flush_all(
        &mut self,
        states: &super::markdown_stream::StateLookup<'_>,
    ) -> Vec<(usize, super::markdown_stream::FenceRef)> {
        let mut out = Vec::new();
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                for fence in stream.flush_final(states) {
                    out.push((idx, fence));
                }
            }
        }
        self.invalidate_cache();
        out
    }

    #[cfg(not(feature = "markdown"))]
    pub fn force_flush_all(&mut self) -> Vec<(usize, ())> {
        Vec::new()
    }

    /// Mark every `AgentMessage` markdown stream as dirty.
    #[cfg(feature = "markdown")]
    pub fn mark_all_streams_dirty(&mut self) {
        for entry in &mut self.entries {
            if let Some(stream) = entry.markdown.as_mut() {
                stream.mark_dirty_now();
            }
        }
        self.invalidate_cache();
    }

    /// Returns true if any entry has a pending permission request.
    pub fn has_pending_permission(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(&e.kind, TraceKind::Permission { pending: true, .. }))
    }

    /// Mark all pending permission entries as resolved.
    pub fn resolve_pending_permissions(&mut self) {
        for entry in &mut self.entries {
            if let TraceKind::Permission { pending, .. } = &mut entry.kind {
                *pending = false;
            }
        }
        self.invalidate_cache();
    }

    /// Collect executor IDs from all Delegate trace entries that have been
    /// dispatched.
    pub fn active_executor_ids(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in &self.entries {
            if let TraceKind::Delegate {
                executor_id: Some(id),
                ..
            } = &e.kind
            {
                if seen.insert(id.as_str()) {
                    out.push(id.clone());
                }
            }
        }
        out
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Expose the trace entries as a slice for testing and inspection.
    pub fn entries(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Test-only accessor.
    #[doc(hidden)]
    pub(crate) fn entries_for_test(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Return the text of the most recent entry, or `None` if the trace is empty.
    #[cfg(test)]
    pub fn last_text(&self) -> Option<String> {
        self.entries.last().map(|e| e.text.clone())
    }

    /// Locate the most recent `Delegate` entry whose `request_id` matches
    /// and attach the `executor_id`.
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
                    self.invalidate_cache();
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

impl ReactTrace {
    /// Render every entry to plain strings (one per logical line).
    pub fn render_to_strings(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let collapsed = self.observe_collapsed;

        let mut i = 0;
        while i < self.entries.len() {
            let entry = &self.entries[i];

            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                } = &entry.kind
                {
                    let (act_glyph, _) = family_glyph(*family);
                    let id_str = input_summary(input, tool);
                    let (tail, consumed) = if let Some(TraceKind::Observe { payload: Some(p) }) =
                        self.entries.get(i + 1).map(|e| &e.kind)
                    {
                        let (obs_glyph, _, stats) = observe_compact(p);
                        let mut t = obs_glyph.to_string();
                        if !stats.is_empty() {
                            t.push(' ');
                            t.push_str(&stats);
                        }
                        (t, 2)
                    } else {
                        ("\u{2026}".to_string(), 1)
                    };
                    lines.push(format!(
                        "{} {} {}  {}",
                        entry.timestamp, act_glyph, id_str, tail
                    ));
                    lines.push(String::new());
                    i += consumed;
                    continue;
                }
            }

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

                TraceKind::Act {
                    tool,
                    family,
                    input,
                } => {
                    let (glyph, _) = family_glyph(*family);
                    lines.push(format!("{} {} {}", entry.timestamp, glyph, tool));
                    if matches!(input, ToolInputDisplay::Empty) {
                        for text_line in entry.text.lines() {
                            lines.push(format!("   {}", text_line));
                        }
                    } else {
                        for l in input_display_lines(input) {
                            let joined: String =
                                l.spans.iter().map(|s| s.content.as_ref()).collect();
                            lines.push(joined);
                        }
                    }
                }

                TraceKind::Observe { payload } => {
                    if let Some(p) = payload {
                        let (glyph, _) = outcome_glyph(p);
                        let verb = observe_verb(p);
                        lines.push(format!("{} {} {}", entry.timestamp, glyph, verb));
                        for l in observe_payload_lines(p, self.observe_collapsed) {
                            let joined: String =
                                l.spans.iter().map(|s| s.content.as_ref()).collect();
                            lines.push(joined);
                        }
                    } else {
                        lines.push(format!("{} 👁 OBSERVE", entry.timestamp));
                        for text_line in entry.text.lines() {
                            lines.push(format!("   {}", text_line));
                        }
                    }
                }

                TraceKind::Delegate {
                    agent,
                    task,
                    status,
                    request_id: _,
                    executor_id: _,
                } => {
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

                TraceKind::Permission {
                    description,
                    pending,
                    countdown,
                } => {
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

            let skip_blank = matches!(&entry.kind, TraceKind::Act { .. })
                && matches!(
                    self.entries.get(i + 1).map(|e| &e.kind),
                    Some(TraceKind::Observe { payload: Some(_) })
                );
            if !skip_blank {
                lines.push(String::new());
            }
            i += 1;
        }

        lines
    }
}

#[cfg(test)]
impl ReactTrace {
    /// Test-only helper: returns each line as a joined `String` of span text.
    pub(crate) fn render_lines_for_test(&self, _width: u16) -> Vec<String> {
        self.render_to_strings()
    }

    pub fn new_for_tests() -> Self {
        Self::new()
    }

    pub fn build_virtual_rows_for_tests(
        &self,
        from: usize,
        width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> (Vec<VirtualRow>, Vec<usize>) {
        self.build_virtual_rows(from, width, states, lineage)
    }

    pub fn entries_for_tests(&self) -> &[TraceEntry] {
        &self.entries
    }

    pub fn build_display_lines_for_tests(
        &self,
        spinner_frame: &str,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> Vec<ratatui::text::Line<'static>> {
        self.build_display_lines(spinner_frame, lineage)
    }
}

#[cfg(all(test, feature = "markdown"))]
mod markdown_integration_tests {
    use super::*;

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

    #[test]
    fn post_flush_rendered_lines_still_show_text() {
        use crate::components::markdown_stream::StateLookup;
        let mut trace = ReactTrace::new();
        // Two paragraphs separated by \n\n with trailing content after the
        // second paragraph so its End event has range.end < raw_text.len(),
        // making it authoritative under flush_now (permit_eof_closure=false).
        trace.append_message("# Heading\n\nBody text\n\nmore", "claude", "10:00:00".to_string());

        let states = StateLookup::empty();
        let _ = trace.force_flush_all(&states);

        let rendered = trace.render_lines_for_test(60);
        let joined = rendered.join("\n");
        assert!(
            joined.contains("Heading"),
            "expected heading text after flush: {joined}"
        );
        assert!(
            joined.contains("Body text"),
            "expected body text after flush: {joined}"
        );
    }

    #[test]
    fn items_path_renders_same_text_as_lines_path() {
        let mut trace = ReactTrace::new();
        // Two paragraphs separated by \n\n with trailing content so the
        // second paragraph's End event has range.end < raw_text.len().
        trace.append_message("# Heading\n\nBody\n\nmore", "claude", "10:00".to_string());
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.force_flush_all(&StateLookup::empty());

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

        let entries = trace.entries_for_test();
        assert!(!entries.is_empty(), "expected at least one entry");
        for entry in entries {
            if let Some(stream) = entry.markdown.as_ref() {
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
    use crate::components::markdown_stream::StateLookup;
    use crate::components::mermaid::FenceRender;
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};

    #[test]
    fn virtual_rows_text_only_match_line_count() {
        let mut trace = ReactTrace::new();
        trace.append_message("Line 1\nLine 2\nLine 3", "claude", "10:00".to_string());
        use crate::components::markdown_stream::StateLookup;
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let total = trace
            .build_virtual_rows(0, 60, &std::collections::HashMap::new(), None)
            .0
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
        let _ = trace.force_flush_all(&StateLookup::empty());
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        use std::collections::HashMap;
        let mut states: HashMap<crate::components::mermaid::MermaidId, FenceRender> =
            HashMap::new();
        states.insert(
            crate::components::mermaid::MermaidId(0),
            FenceRender::Ready(12),
        );

        let (rows, _starts) = trace.build_virtual_rows(0, 60, &states, None);

        let image_rows: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                VirtualRow::ImageRow {
                    id,
                    row_within,
                    total_rows,
                } => Some((*id, *row_within, *total_rows)),
                _ => None,
            })
            .collect();

        assert_eq!(
            image_rows.len(),
            12,
            "expected 12 image rows; got {image_rows:?}"
        );
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
        let (rows, _starts) = trace.build_virtual_rows(0, 60, &empty, None);

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
        states.insert(
            crate::components::mermaid::MermaidId(0),
            FenceRender::Ready(8),
        );

        let segments = trace.render_plan_for_test(80, 40, 0, &states);

        let image_segs: Vec<_> = segments
            .iter()
            .filter(|s| matches!(s, Segment::Image { .. }))
            .collect();
        assert_eq!(
            image_segs.len(),
            1,
            "expected exactly one image segment: {segments:?}"
        );
        if let Segment::Image {
            total_rows,
            first_row_within,
            run_len,
            ..
        } = image_segs[0]
        {
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
        states.insert(
            crate::components::mermaid::MermaidId(0),
            FenceRender::Ready(10),
        );

        let mut saw_partial = false;
        for offset in 0..20 {
            let segs = trace.render_plan_for_test(80, 6, offset, &states);
            if segs.iter().any(|s| {
                matches!(
                    s,
                    Segment::Image { total_rows, run_len, .. } if run_len < total_rows
                )
            }) {
                saw_partial = true;
                break;
            }
        }
        assert!(
            saw_partial,
            "expected some offset to produce partial image segment"
        );
    }

    #[test]
    fn entry_row_starts_remain_indexed_by_absolute_entry_after_collapsed_pairs() {
        let mut trace = ReactTrace::new();
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "shell".to_string(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "echo hi".to_string(),
                    cwd: None,
                },
            },
            text: String::new(),
            timestamp: "10:00".to_string(),
            markdown: None,
        });
        trace.push(TraceEntry {
            kind: TraceKind::Observe {
                payload: Some(ObservePayload::Text {
                    body: "hi".to_string(),
                }),
            },
            text: String::new(),
            timestamp: "10:00".to_string(),
            markdown: None,
        });
        trace.append_message("first line", "claude", "10:01".to_string());
        let _ = trace.force_flush_all(&StateLookup::empty());

        let (_rows, starts) =
            trace.build_virtual_rows(0, 80, &std::collections::HashMap::new(), None);

        assert_eq!(
            starts.len(),
            trace.entries().len(),
            "entry row starts must stay aligned with absolute entry indices"
        );
        assert!(
            starts.get(2).is_some(),
            "the streamed AgentMessage entry must retain a start offset even when earlier Act+Observe pairs are collapsed"
        );
    }
}

#[cfg(test)]
mod streaming_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::adapter::ToolFamily;

    /// Post-tool text must appear as a SEPARATE AgentMessage entry, not
    /// merged into the pre-tool block.
    #[test]
    fn append_message_creates_new_entry_after_tool_call() {
        let mut trace = ReactTrace::new();
        trace.append_message("first chunk. ", "claude", "10:00:01".to_string());
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "read_file".to_string(),
                family: ToolFamily::Unknown,
                input: ToolInputDisplay::Empty,
            },
            text: "read_file(path=...)".to_string(),
            timestamp: "10:00:02".to_string(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        trace.append_message("second chunk.", "claude", "10:00:03".to_string());

        let entries = trace.entries_for_test();
        let agent_messages: Vec<_> = entries
            .iter()
            .filter(|e| matches!(&e.kind, TraceKind::AgentMessage { agent } if agent == "claude"))
            .collect();
        assert_eq!(
            agent_messages.len(),
            2,
            "expected 2 separate AgentMessage entries (pre-tool and post-tool), got {}",
            agent_messages.len()
        );

        let kinds: Vec<&str> = entries
            .iter()
            .map(|e| match &e.kind {
                TraceKind::AgentMessage { .. } => "agent_message",
                TraceKind::Act { .. } => "act",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["agent_message", "act", "agent_message"],
            "trace should be [msg, tool, msg] but got {kinds:?}"
        );
    }

    /// Consecutive AgentMessageChunks must still merge into a single entry.
    #[test]
    fn append_message_merges_consecutive_chunks_from_same_agent() {
        let mut trace = ReactTrace::new();
        trace.append_message("hello ", "claude", "10:00:01".to_string());
        trace.append_message("world", "claude", "10:00:01".to_string());

        let entries = trace.entries_for_test();
        let agent_message_count = entries
            .iter()
            .filter(|e| matches!(&e.kind, TraceKind::AgentMessage { agent } if agent == "claude"))
            .count();
        assert_eq!(
            agent_message_count, 1,
            "consecutive chunks should merge into one entry"
        );
    }

    #[test]
    fn append_user_message_creates_new_entry_when_empty() {
        let mut trace = ReactTrace::new();
        trace.append_user_message("hello", "10:00:01".to_string());
        let entries = trace.entries_for_test();
        let user_count = entries
            .iter()
            .filter(|e| matches!(&e.kind, TraceKind::UserMessage))
            .count();
        assert_eq!(user_count, 1);
        assert_eq!(entries.last().unwrap().text, "hello");
    }

    #[test]
    fn append_user_message_coalesces_into_tail_user_entry() {
        let mut trace = ReactTrace::new();
        trace.push(TraceEntry {
            kind: TraceKind::UserMessage,
            text: "hello ".to_string(),
            timestamp: "10:00:01".to_string(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        trace.append_user_message("world", "10:00:02".to_string());

        let entries = trace.entries_for_test();
        let user_entries: Vec<_> = entries
            .iter()
            .filter(|e| matches!(&e.kind, TraceKind::UserMessage))
            .collect();
        assert_eq!(user_entries.len(), 1, "must coalesce, not duplicate");
        assert_eq!(user_entries[0].text, "hello world");
    }

    #[test]
    fn append_user_message_idempotent_when_text_already_seeded() {
        let mut trace = ReactTrace::new();
        // Simulate HistoryEntry path seeding the full turn.
        trace.push(TraceEntry {
            kind: TraceKind::UserMessage,
            text: "list the files in src/".to_string(),
            timestamp: "10:00:01".to_string(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        // ACP protocol later replays the same text as a chunk.
        trace.append_user_message("list the files in src/", "10:00:02".to_string());

        let entries = trace.entries_for_test();
        let user: Vec<_> = entries
            .iter()
            .filter(|e| matches!(&e.kind, TraceKind::UserMessage))
            .collect();
        assert_eq!(user.len(), 1);
        assert_eq!(
            user[0].text,
            "list the files in src/",
            "must not double the seeded text"
        );
    }
}
