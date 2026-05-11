use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::ToolCallId;

use ratatui::text::Line;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceImageId(pub u64);

#[derive(Debug, Clone)]
pub struct TraceImage {
    pub image: Arc<image::DynamicImage>,
    pub path: PathBuf,
    pub image_generation: u64,
}

/// Terminal/non-terminal state of a tool call.
///
/// Mirrors `agent_client_protocol::ToolCallStatus` but embeds the outcome
/// payload directly so a single `TraceEntry` represents the full lifecycle
/// of one tool call. Non-terminal variants keep the spinner animating;
/// terminal variants render the outcome glyph.
#[derive(Debug, Clone)]
pub enum ActStatus {
    Pending,
    InProgress {
        /// Streamed partial output. Stored but NOT rendered in Phase 1.
        partial: Option<ObservePayload>,
    },
    Completed(Option<ObservePayload>),
    Failed(Option<ObservePayload>),
}

impl ActStatus {
    /// True when the spinner should keep animating.
    pub fn is_active(&self) -> bool {
        matches!(self, ActStatus::Pending | ActStatus::InProgress { .. })
    }
}

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
        /// ACP-originated calls carry their protocol id; synthetic or
        /// test-generated Acts may use `None`.
        tool_call_id: Option<ToolCallId>,
        /// Drives spinner vs. outcome rendering.
        status: ActStatus,
    },
    /// Informational notes only (system, brain events). Tool-call lifecycle
    /// lives on `Act.status` — do not use `Observe` for tool outcomes.
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
    Image {
        id: TraceImageId,
        label: String,
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
        source: InlineImageSource,
        row_within: u16,
        total_rows: u16,
    },
}

#[cfg(feature = "markdown")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InlineImageSource {
    Mermaid(crate::components::mermaid::MermaidId),
    Trace(TraceImageId),
}

#[cfg(feature = "markdown")]
pub struct RenderContext<'a> {
    pub mermaid_registry: &'a std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    pub picker: Option<&'a ratatui_image::picker::Picker>,
    pub image_cache: &'a mut crate::components::image_cache::ImageCache,
}

#[cfg(feature = "markdown")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment {
    Text {
        start: usize,
        len: usize,
    },
    Image {
        source: InlineImageSource,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollAnchor {
    #[default]
    Following,
    Row {
        entry_idx: usize,
        row_within_entry: usize,
    },
}
