//! INV-alpha merge-budget constants shared between the materializer (spur-mcp)
//! and the merger (spur-core). Held in spur-acp so spur-mcp can reference
//! them without introducing a spur-mcp -> spur-core cycle.
//!
//! The exact rendered-cost arithmetic (`block_byte_cost`,
//! `continuation_resource_block`) stays in spur-core because it depends
//! on `agent-client-protocol` types (`ContentBlock`, `EmbeddedResource`).
//! The materializer uses a conservative upper-bound cost estimate (see
//! `spur_mcp::outcome_materializer::estimate_envelope_cost`) and the
//! merger's `pack_continuations` is the authoritative INV-alpha gate.

pub const MERGE_BUDGET_DEFAULT_BYTES: usize = 8192;

/// Headroom reserved by the materializer for the JSON-RPC wrapper
/// (`uri`, `mime_type`, `EmbeddedResource` envelope). Empirically the
/// rendered envelope adds about 256 B over `serde_json::to_vec(payload).len()`;
/// 1024 is comfortable headroom and still leaves more than 7 KiB for payload.
pub const ENVELOPE_WRAPPER_HEADROOM_BYTES: usize = 1024;
