//! Compatibility re-export of the shared code-mention expansion (sud-m4).
//!
//! Discovery/validation/expansion live in `spur_mentions::code::expansion`
//! behind the `code` feature; this path (`crate::mentions::code_graph::
//! expansion`) stays stable for the submit router and integration tests
//! (decoupling spec §3.4, §4.5 — expansion stays callable from the TUI).

pub use spur_mentions::code::expansion::{
    expand, failure_reason_label, ExpandedMention, ReplacedWith, CONTEXT_HEADER_CAP_BYTES,
    PER_PROMPT_CAP_BYTES,
};
