use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PrParams};
use async_trait::async_trait;

/// Trait for PM tool adapters (GitHub, Linear, Plane).
#[async_trait]
pub trait PmAdapter: Send + Sync {
    /// Authenticate and connect to the PM service.
    async fn connect(&mut self) -> anyhow::Result<()>;

    /// Fetch a single issue by ID.
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue>;

    /// List issues matching a filter.
    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>>;

    /// Update an issue (status, comment, labels).
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()>;

    /// Create a pull request.
    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String>;

    /// Poll for new/updated issues since last check.
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>>;
}
