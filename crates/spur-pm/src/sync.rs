use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The contract Spur uses to talk to any external PM. Separate from
/// `IssueTracker` so external systems cannot become peer authorities (I-7).
#[async_trait]
pub trait ExternalPmSync: Send + Sync {
    /// Stable provenance tag - "github", "linear", "plane".
    fn source_system(&self) -> &'static str;

    /// Per-instance scope, e.g. "getspur/spur".
    fn source_repo(&self) -> &str;

    /// Bulk pull. `since=None` means full repo state.
    async fn fetch_changes_since(&self, since: Option<DateTime<Utc>>) -> SyncResult<RemoteDelta>;

    /// Fetch a single remote node by stable id. `if_none_match` is the
    /// REST-only fast path; GraphQL implementations ignore it.
    async fn fetch_one(
        &self,
        remote_id: &str,
        if_none_match: Option<&str>,
    ) -> SyncResult<FetchOneOutcome>;

    /// Project local Beads mutations onto the remote.
    /// `Vec` order is preserved; outcomes align positionally.
    async fn push_mutations(&self, diff: Vec<LocalMutation>) -> SyncResult<Vec<PushOutcome>>;

    /// Compare local watermarks against the remote (cheap; uses ETag/
    /// updated_at). Used by the apply step before any push.
    async fn detect_conflicts(
        &self,
        watermarks: &[SyncWatermark],
    ) -> SyncResult<Vec<RemoteConflict>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDelta {
    pub nodes: Vec<RemoteNode>,
    /// Remote IDs known to be deleted/inaccessible.
    pub deletions: Vec<RemoteRef>,
    /// Server-time cursor for the next `fetch_changes_since` call.
    pub watermark: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNode {
    pub remote_id: String,          // e.g. GitHub node_id: "I_kwDO..."
    pub remote_number: Option<u64>, // e.g. issue #42 (display only)
    pub kind: RemoteKind,
    pub title: String,
    pub body: String,
    pub state: RemoteState,
    pub labels: Vec<String>,    // raw remote names; mapping happens later
    pub assignees: Vec<String>, // remote logins
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub html_url: String,
    pub etag: Option<String>, // REST poll path only
    pub dep_hints: Vec<DepHint>,
    pub comments: Vec<RemoteComment>,
    /// Anything we didn't map; preserved for forward-compat.
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RemoteKind {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RemoteState {
    Open,
    Closed { reason: Option<String> },
    Draft, // PRs only
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRef {
    pub source_system: String,
    pub remote_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteComment {
    pub remote_id: String, // GitHub comment node_id
    pub author: String,    // login
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepHint {
    pub kind: DepHintKind,
    pub remote_keyword: String,         // verbatim, e.g. "Closes"
    pub remote_ref: String,             // canonical form, "owner/repo#42"
    pub remote_node_id: Option<String>, // when GraphQL gave us the resolved node_id
    pub raw_span: String,               // exact source text
    pub source: DepHintSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DepHintKind {
    Closes,
    Fixes,
    Resolves,
    DependsOn,
    Blocks,
    BlockedBy,
    TaskList,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DepHintSource {
    Body,
    TimelineItem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncWatermark {
    pub beads_id: String,
    pub remote_id: String,
    pub last_synced_at: DateTime<Utc>,
    pub last_synced_etag: Option<String>,
    pub last_synced_remote_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FetchOneOutcome {
    Unchanged, // 304 / etag match
    Updated(RemoteNode),
    Gone, // 404 / repo private / transferred
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalMutation {
    pub beads_id: String,
    pub remote_id: String,
    pub kind: LocalMutationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LocalMutationKind {
    StatusChange { from: String, to: String },
    LabelsAdded(Vec<String>),
    LabelsRemoved(Vec<String>),
    CommentAdded { body: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PushOutcome {
    Pushed {
        new_etag: Option<String>,
        new_remote_updated_at: DateTime<Utc>,
    },
    Conflict(RemoteConflict),
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConflict {
    pub beads_id: String,
    pub remote_id: String,
    pub local_updated_at: DateTime<Utc>,
    pub remote_updated_at: DateTime<Utc>,
    pub watermark_remote_updated_at: DateTime<Utc>,
    pub reason: ConflictReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ConflictReason {
    RemoteMovedSinceLastSync,
    LocalAndRemoteBothMutated,
}

pub type SyncResult<T> = std::result::Result<T, SyncError>;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("authentication required: {0}")]
    NeedsAuth(String),
    #[error("remote not found: {0}")]
    Gone(String),
    #[error("rate limited; retry after {retry_after_s}s")]
    RateLimited { retry_after_s: u64 },
    #[error("transient network error: {0}")]
    Transient(String),
    #[error("malformed remote response: {0}")]
    Malformed(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod mock {
    use std::collections::HashMap;

    use chrono::Utc;

    use super::*;

    /// In-memory `ExternalPmSync` implementation for downstream ingest tests.
    #[derive(Debug, Clone)]
    pub struct MockSync {
        pub source_system: &'static str,
        pub source_repo: String,
        pub delta: RemoteDelta,
        pub fetch_one_outcomes: HashMap<String, FetchOneOutcome>,
        pub push_outcomes: Vec<PushOutcome>,
        pub conflicts: Vec<RemoteConflict>,
    }

    impl MockSync {
        pub fn new(source_repo: impl Into<String>) -> Self {
            Self {
                source_system: "github",
                source_repo: source_repo.into(),
                delta: RemoteDelta {
                    nodes: Vec::new(),
                    deletions: Vec::new(),
                    watermark: Utc::now(),
                },
                fetch_one_outcomes: HashMap::new(),
                push_outcomes: Vec::new(),
                conflicts: Vec::new(),
            }
        }

        pub fn with_delta(mut self, delta: RemoteDelta) -> Self {
            self.delta = delta;
            self
        }

        pub fn with_fetch_one(
            mut self,
            remote_id: impl Into<String>,
            outcome: FetchOneOutcome,
        ) -> Self {
            self.fetch_one_outcomes.insert(remote_id.into(), outcome);
            self
        }

        pub fn with_push_outcomes(mut self, outcomes: Vec<PushOutcome>) -> Self {
            self.push_outcomes = outcomes;
            self
        }

        pub fn with_conflicts(mut self, conflicts: Vec<RemoteConflict>) -> Self {
            self.conflicts = conflicts;
            self
        }
    }

    #[async_trait]
    impl ExternalPmSync for MockSync {
        fn source_system(&self) -> &'static str {
            self.source_system
        }

        fn source_repo(&self) -> &str {
            &self.source_repo
        }

        async fn fetch_changes_since(
            &self,
            _since: Option<DateTime<Utc>>,
        ) -> SyncResult<RemoteDelta> {
            Ok(self.delta.clone())
        }

        async fn fetch_one(
            &self,
            remote_id: &str,
            _if_none_match: Option<&str>,
        ) -> SyncResult<FetchOneOutcome> {
            Ok(self
                .fetch_one_outcomes
                .get(remote_id)
                .cloned()
                .unwrap_or(FetchOneOutcome::Gone))
        }

        async fn push_mutations(&self, diff: Vec<LocalMutation>) -> SyncResult<Vec<PushOutcome>> {
            if self.push_outcomes.is_empty() {
                return Ok(diff
                    .into_iter()
                    .map(|mutation| PushOutcome::Skipped {
                        reason: format!("no mock outcome for {}", mutation.beads_id),
                    })
                    .collect());
            }

            Ok(self.push_outcomes.clone())
        }

        async fn detect_conflicts(
            &self,
            _watermarks: &[SyncWatermark],
        ) -> SyncResult<Vec<RemoteConflict>> {
            Ok(self.conflicts.clone())
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub use mock::MockSync;
