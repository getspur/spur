//! Typed mention error taxonomy (2026-08-27 mentions spec §2 and the
//! error-handling table): root, traversal, source-build, ranking, and
//! code-hydrate failures are distinct categories attributable to exactly
//! one boundary. Failures are never flattened into an empty picker:
//! required sources fail the query with these types, optional ones degrade
//! to [`QueryDiagnostic`]s.
//!
//! Messages carried here are returned to the calling frontend, which owns
//! their display; the engine's tracing spans stay redacted (no paths, no
//! query text).

use std::fmt;

/// Invalid or missing canonical workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootError {
    /// Redacted diagnostic (error class only; the caller already knows the
    /// path it passed).
    pub message: String,
}

impl RootError {
    /// Build the error from a diagnostic message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mention root error: {}", self.message)
    }
}

impl std::error::Error for RootError {}

/// A source's filesystem traversal failed during a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalError {
    /// Source the traversal belongs to.
    pub source_key: String,
    /// Redacted diagnostic message.
    pub message: String,
}

impl TraversalError {
    /// Build the error for `source_key`.
    pub fn new(source_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source_key: source_key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for TraversalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mention traversal failure (source {}): {}",
            self.source_key, self.message
        )
    }
}

impl std::error::Error for TraversalError {}

/// A required source failed to build its snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBuildFailure {
    /// Source that failed.
    pub source_key: String,
    /// Diagnostic message from the source.
    pub message: String,
}

impl SourceBuildFailure {
    /// Build the failure for `source_key`.
    pub fn new(source_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source_key: source_key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceBuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mention source build failure (source {}): {}",
            self.source_key, self.message
        )
    }
}

impl std::error::Error for SourceBuildFailure {}

/// Ranking failed for this query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankError {
    /// Redacted diagnostic message.
    pub message: String,
}

impl RankError {
    /// Build the error from a diagnostic message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mention ranking failure: {}", self.message)
    }
}

impl std::error::Error for RankError {}

/// Hydration of a selected code-graph row failed (constructed by the
/// `code` module once it moves; typed now so the taxonomy is complete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHydrationFailure {
    /// Source the hydration belongs to.
    pub source_key: String,
    /// Redacted diagnostic message.
    pub message: String,
}

impl CodeHydrationFailure {
    /// Build the failure for `source_key`.
    pub fn new(source_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source_key: source_key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for CodeHydrationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mention code hydration failure (source {}): {}",
            self.source_key, self.message
        )
    }
}

impl std::error::Error for CodeHydrationFailure {}

/// Typed engine failure. Categories follow the 2026-08-27 spec taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MentionError {
    /// Invalid or missing canonical root.
    Root(RootError),
    /// A source's filesystem traversal failed.
    Traversal(TraversalError),
    /// A required source failed to build.
    SourceBuild(SourceBuildFailure),
    /// Ranking failed.
    Rank(RankError),
    /// Code payload hydration failed for a selected row.
    CodeHydration(CodeHydrationFailure),
}

impl fmt::Display for MentionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(error) => error.fmt(f),
            Self::Traversal(error) => error.fmt(f),
            Self::SourceBuild(failure) => failure.fmt(f),
            Self::Rank(error) => error.fmt(f),
            Self::CodeHydration(failure) => failure.fmt(f),
        }
    }
}

impl std::error::Error for MentionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Root(error) => Some(error),
            Self::Traversal(error) => Some(error),
            Self::SourceBuild(failure) => Some(failure),
            Self::Rank(error) => Some(error),
            Self::CodeHydration(failure) => Some(failure),
        }
    }
}

/// Non-fatal degradation of one optional source: the query still returned,
/// but that source contributed nothing (or only its previously retained
/// snapshot) for the stated reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDiagnostic {
    /// Source that degraded.
    pub source_key: String,
    /// Redacted diagnostic message.
    pub message: String,
}

impl fmt::Display for QueryDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "source {} degraded: {}", self.source_key, self.message)
    }
}
