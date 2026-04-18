use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};

use ratatui::text::Line;

/// What kind of ReAct trace step this entry represents.
#[derive(Debug, Clone)]
pub enum TraceKind {
    Think,
    AgentMessage {
        agent: String,
    },
    Act {
        tool: String,
        family: ToolFamily,
        input: ToolInputDisplay,
    },
    Observe {
        payload: Option<ObservePayload>,
    },
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
    Permission {
        description: String,
        pending: bool,
        countdown: u8,
    },
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
    pub markdown: Option<crate::components::markdown_stream::MarkdownStream>,
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

/// Anchor model for the trace viewport.
///
/// `Following` tracks the bottom of the document.
/// `Row` pins the viewport to ordinal row `row_within_entry` of the entry
/// at `entry_idx`. Resolved at render time via `entry_row_starts`. Width
/// resize clamps to the entry's last row (Phase 3 trade-off; v1 byte
/// anchor was entry-coarse and snapped to entry start anyway).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAnchor {
    Following,
    Row {
        entry_idx: usize,
        row_within_entry: usize,
    },
}

impl Default for ScrollAnchor {
    fn default() -> Self {
        ScrollAnchor::Following
    }
}
