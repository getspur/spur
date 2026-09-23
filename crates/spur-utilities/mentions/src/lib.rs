//! Neutral mention completion engine (2026-08-27 mentions spec).
//!
//! No ACP types cross this crate's API. The optional `code` feature adds the
//! `spur-graph`-backed code discovery/expansion module; the TUI enables it,
//! the notebook does not.
//!
//! M1 (decoupling spec §5 Phase M) ships the neutral core types
//! ([`MentionId`], [`MentionEntry`], [`MentionKind`], [`MentionSource`],
//! [`SourceSnapshot`]); the engine, cache, ranker, and file source move in
//! M2+.

pub mod entry;

pub use entry::{
    MentionEntry, MentionId, MentionKind, MentionSource, SourceBuildError, SourceContext,
    SourceSnapshot,
};

/// Graph-backed code mention payload, available only when the `code` feature
/// pulls `spur-graph` (the TUI enables it; the notebook's
/// `default-features = false` build never sees it).
#[cfg(feature = "code")]
pub use spur_graph::CodeMentionPayload;
