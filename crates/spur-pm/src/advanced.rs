//! Beads-only extension surface.
//!
//! These methods expose `br` CLI primitives that have no GitHub-backend
//! analog (ready, comment CRUD, dep cycles). Only `BeadsAdapter`
//! implements this trait. Callers obtain a `&dyn BeadsAdvanced` from
//! `PmService::advanced()`, which returns `None` for non-beads backends.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::sync::DepHint;
use crate::types::IssueSummary;

// ─── Filter & input types ─────────────────────────────────────────────

/// Filter passed to `BeadsAdvanced::list_ready`. Mirrors the actual flag
/// surface of `br ready` as of br 0.1.14 rather than inventing a
/// caller-convenient shape that lies about the backend's semantics.
///
/// `priorities` is a **set-membership** filter matching br's empirically
/// verified `-p, --priority <PRIORITY>  (can be repeated, 0-4 or P0-P4)`
/// model: `br ready -p 0 -p 2` returns P0 ∪ P2. Empty vec = no priority
/// filter. To express a contiguous range, enumerate:
/// `priorities: vec![2, 3, 4]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyFilter {
    pub assignee: Option<String>,
    pub labels_all: Vec<String>,
    pub labels_any: Vec<String>,
    pub issue_type: Option<String>,
    /// Set of priorities to include (repeated `-p <n>` flags). Empty = no filter.
    pub priorities: Vec<i32>,
    pub limit: Option<usize>,
}

// ─── Output types ─────────────────────────────────────────────────────

pub type CommentId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub body: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyCycle {
    /// Issue IDs forming the cycle, in dependency order.
    pub issues: Vec<String>,
}

/// A dep hint parsed from a `spur-dep-hint v1` sentinel, with a
/// live-resolved local `beads_id` if the referenced remote node has
/// already been ingested. Read-only; the brain consumes these and
/// decides whether to call `IssueTracker::add_dependency`.
#[derive(Debug, Clone)]
pub struct ResolvedDepHint {
    pub hint: DepHint,
    pub resolved_beads_id: Option<String>,
}

// ─── Trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait BeadsAdvanced: Send + Sync {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>>;

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>>;

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId>;

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()>;

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>>;

    /// List dep-hint sentinels on an issue, with live resolution
    /// against the current `external_ref` index (§5.6). Read-only;
    /// never mutates. The default impl returns an empty vec so
    /// non-Beads backends compile; Beads-backed implementations
    /// override with the real read.
    async fn list_dep_hints(&self, _issue_id: &str) -> anyhow::Result<Vec<ResolvedDepHint>> {
        Ok(Vec::new())
    }
}

// ─── Unit tests for type serialization ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_filter_default_is_empty() {
        let f = ReadyFilter::default();
        assert!(f.assignee.is_none());
        assert!(f.labels_all.is_empty());
        assert!(f.labels_any.is_empty());
        assert!(f.priorities.is_empty());
        assert!(f.limit.is_none());
    }
}
