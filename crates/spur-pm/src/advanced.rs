//! Beads-only extension surface.
//!
//! These methods expose `br` CLI primitives that have no GitHub-backend
//! analog (ready, audit, comment CRUD, dep cycles). Only `BeadsAdapter`
//! implements this trait. Callers obtain a `&dyn BeadsAdvanced` from
//! `PmService::advanced()`, which returns `None` for non-beads backends.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecordInput {
    pub entry_type: AuditEntryType,
    pub data: serde_json::Value,
}

// ─── Closed vocabulary for audit entry types ──────────────────────────

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditEntryType {
    PlanSubmit,
    Dispatch,
    Completion,
    Approval,
    Rejection,
    Signal,
    MutationPlan,
    MutationCommit,
    MutationInvariantViolation,
    MutationCancelled,
    LateSignal,
    OrphanDepDetected,
}

// ─── Output types ─────────────────────────────────────────────────────

pub type AuditId = String;
pub type CommentId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: AuditId,
    pub issue_id: String,
    pub entry_type: AuditEntryType,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

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

// ─── Trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait BeadsAdvanced: Send + Sync {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>>;

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>>;

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId>;

    async fn audit_record(
        &self,
        issue_id: &str,
        entry: AuditRecordInput,
    ) -> anyhow::Result<AuditId>;

    async fn audit_log(&self, issue_id: &str) -> anyhow::Result<Vec<AuditEntry>>;

    async fn remove_dependency(
        &self,
        issue_id: &str,
        depends_on_id: &str,
    ) -> anyhow::Result<()>;

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>>;
}

// ─── Unit tests for type serialization ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_entry_type_serializes_kebab_case() {
        let t = AuditEntryType::MutationPlan;
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "\"mutation-plan\"");
    }

    #[test]
    fn audit_entry_type_round_trips() {
        for t in [
            AuditEntryType::PlanSubmit,
            AuditEntryType::Dispatch,
            AuditEntryType::Completion,
            AuditEntryType::Approval,
            AuditEntryType::Rejection,
            AuditEntryType::Signal,
            AuditEntryType::MutationPlan,
            AuditEntryType::MutationCommit,
            AuditEntryType::MutationInvariantViolation,
            AuditEntryType::MutationCancelled,
            AuditEntryType::LateSignal,
            AuditEntryType::OrphanDepDetected,
        ] {
            let s = serde_json::to_string(&t).unwrap();
            let back: AuditEntryType = serde_json::from_str(&s).unwrap();
            assert_eq!(t, back);
        }
    }

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
