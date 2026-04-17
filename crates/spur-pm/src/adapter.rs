use async_trait::async_trait;

use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PrParams};

#[async_trait]
pub trait IssueTracker: Send + Sync {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()>;
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>>;
}

#[async_trait]
pub trait PrService: Send + Sync {
    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String>;
}
