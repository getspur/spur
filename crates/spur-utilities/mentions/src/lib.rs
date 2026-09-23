//! Neutral mention completion engine (2026-08-27 mentions spec).
//!
//! No ACP types cross this crate's API. The optional `code` feature adds the
//! `spur-graph`-backed code discovery/expansion module; the TUI enables it,
//! the notebook does not.
//!
//! M1 (decoupling spec §5 Phase M) shipped the neutral core types
//! ([`MentionEntry`], [`MentionId`], [`MentionKind`], [`MentionSource`],
//! [`SourceSnapshot`]). M2a adds the engine core: [`MentionEngine::query`],
//! the root-scoped TTL [`cache::CacheKey`] identity, the filesystem
//! compatibility [`FilesystemProfile`]s behind [`FileMentionSource`], and
//! the deterministic [`rank_full_sort`] reference the later top-K
//! selection must reproduce.

pub mod cache;
pub mod clock;
pub mod engine;
pub mod entry;
pub mod error;
pub mod file_source;
pub mod profile;
pub mod rank;

pub use cache::CacheKey;
pub use clock::{Clock, ManualClock, SystemClock};
pub use engine::{MentionEngine, QueryOptions, QueryResult, DEFAULT_SNAPSHOT_TTL};
pub use entry::{
    MentionEntry, MentionId, MentionKind, MentionSource, SourceBuildError, SourceBuildErrorKind,
    SourceContext, SourceSnapshot,
};
pub use error::{
    CodeHydrationFailure, MentionError, QueryDiagnostic, RankError, RootError, SourceBuildFailure,
    TraversalError,
};
pub use file_source::FileMentionSource;
pub use profile::{FilesystemProfile, UriConstruction};
pub use rank::{rank_full_sort, RankOptions, RankedRef, TierPolicy};

/// Graph-backed code mention payload, available only when the `code` feature
/// pulls `spur-graph` (the TUI enables it; the notebook's
/// `default-features = false` build never sees it).
#[cfg(feature = "code")]
pub use spur_graph::CodeMentionPayload;
