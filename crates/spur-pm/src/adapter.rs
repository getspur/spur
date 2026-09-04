use async_trait::async_trait;

use crate::types::{
    AtomicUpdateOutcome, AtomicUpdatePrecondition, Issue, IssueCreate, IssueFilter, IssueSummary,
    IssueUpdate, PmEvent, PrParams,
};

#[async_trait]
pub trait IssueTracker: Send + Sync {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>>;
    async fn find_by_external_ref(&self, _external_ref: &str) -> anyhow::Result<Option<Issue>> {
        Ok(None)
    }
    /// Create a new issue. Returns the new issue ID.
    async fn create_issue(&self, params: IssueCreate) -> anyhow::Result<String>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()>;
    /// Apply an ordered set of issue updates as one durable transaction.
    ///
    /// Implementations must not fall back to sequential writes. The same
    /// idempotency key may be retried only with the same update payload.
    /// Preconditions apply to the first commit attempt and must be checked
    /// inside the transaction, after an idempotency-marker lookup, before
    /// applying any update.
    async fn update_issues_atomically(
        &self,
        _idempotency_key: &str,
        _preconditions: Vec<AtomicUpdatePrecondition>,
        _updates: Vec<(String, IssueUpdate)>,
    ) -> anyhow::Result<AtomicUpdateOutcome> {
        anyhow::bail!("atomic multi-issue updates are not supported by this PM backend")
    }
    /// Add a dependency: `issue_id` depends on (is blocked by) `depends_on_id`.
    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()>;
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>>;
}

#[async_trait]
pub trait PrService: Send + Sync {
    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String>;
}
