//! Graph-backed code mention discovery, hydration, and expansion (2026-08-27
//! mentions spec, shared-crate `code` module).
//!
//! Both modules exist only behind the `code` feature (the TUI enables it;
//! the notebook's `default-features = false` build never sees them):
//!
//! - [`source`]: artifact discovery, index loading and compaction, eager
//!   file rows/payloads, and lazy symbol payload hydration.
//! - [`expansion`]: validated expansion of a [`spur_graph::CodeMentionPayload`]
//!   into the body or warning text a prompt consumes.

pub mod expansion;
pub mod source;
