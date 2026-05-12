//! Hand-rolled response types matching `ingest_repo.graphql`.
//!
//! Why hand-rolled, not `graphql_client`'s `#[derive(GraphQLQuery)]`:
//! `graphql_client` requires the full GitHub schema (~50 MB SDL) to be
//! checked into the workspace for codegen. The shape is small enough
//! (a single repository query with two connections) that maintaining
//! these structs by hand is cheaper than vendoring the schema and
//! wiring a build.rs. The `.graphql` file is preserved verbatim per
//! spec §7.3 and remains the source of truth for the wire payload.
//!
//! All fields use serde camelCase to match GraphQL field names. The
//! query string itself is loaded at runtime as `&str` and posted via
//! `octocrab.graphql()`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const INGEST_REPO_QUERY: &str = include_str!("ingest_repo.graphql");

/// Variables for `IngestRepo`. The two cursors are independent — see
/// spec §7.3 P-1: connection cursors are connection-specific opaque
/// strings; the issues `endCursor` cannot be passed to `pullRequests`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRepoVariables {
    pub owner: String,
    pub repo: String,
    pub issue_cursor: Option<String>,
    pub pr_cursor: Option<String>,
    pub issue_page_size: i32,
    pub pr_page_size: i32,
    pub comments_first: i32,
    pub timeline_first: i32,
}

impl IngestRepoVariables {
    /// Spec §7.3 P-2 default page sizes (issue=25, pr=25, comments=30, timeline=20).
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            issue_cursor: None,
            pr_cursor: None,
            issue_page_size: 25,
            pr_page_size: 25,
            comments_first: 30,
            timeline_first: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRepoData {
    pub repository: Option<RepositoryNode>,
    // `rateLimit` is a top-level Query field in GitHub's GraphQL schema,
    // NOT a field on `Repository`. Initial spec placed it under repository
    // and the API rejected the query.
    pub rate_limit: Option<RateLimit>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryNode {
    pub issues: IssueConnection,
    pub pull_requests: PrConnection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueConnection {
    pub page_info: PageInfo,
    pub nodes: Vec<IssueNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrConnection {
    pub page_info: PageInfo,
    pub nodes: Vec<PrNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueNode {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub author: Option<Actor>,
    pub assignees: ActorConnection,
    pub labels: LabelConnection,
    pub comments: CommentConnection,
    pub timeline_items: TimelineConnection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrNode {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub author: Option<Actor>,
    pub assignees: ActorConnection,
    pub labels: LabelConnection,
    pub comments: CommentConnection,
    pub closing_issues_references: ClosingRefConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Actor {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActorConnection {
    pub nodes: Vec<Actor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelConnection {
    pub nodes: Vec<Label>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentConnection {
    pub page_info: PageInfo,
    pub nodes: Vec<CommentNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentNode {
    pub id: String,
    pub author: Option<Actor>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimelineConnection {
    pub nodes: Vec<TimelineItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "__typename")]
pub enum TimelineItem {
    CrossReferencedEvent {
        source: Option<TimelineSource>,
    },
    ClosedEvent {
        #[serde(rename = "stateReason")]
        state_reason: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "__typename")]
pub enum TimelineSource {
    Issue {
        id: String,
        number: u64,
        repository: RepoRef,
    },
    PullRequest {
        id: String,
        number: u64,
        repository: RepoRef,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
    pub name_with_owner: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClosingRefConnection {
    pub nodes: Vec<ClosingIssueRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClosingIssueRef {
    pub id: String,
    pub number: u64,
    pub repository: RepoRef,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub cost: u32,
    pub remaining: u32,
    pub reset_at: DateTime<Utc>,
    pub node_count: u32,
}
