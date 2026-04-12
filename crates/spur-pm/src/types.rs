use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Source of a project management issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    GitHub,
    Linear,
    Plane,
}

/// An issue from a PM tool, normalized to a common format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub status: String,
    pub linked_prs: Vec<String>,
    pub url: String,
}

/// Parameters for creating a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrParams {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: Option<String>,
    pub repo: Option<String>,
}

/// Filter for listing issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

/// Summary of an issue (for list views).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub labels: Vec<String>,
    pub status: String,
    pub url: String,
}

/// Update to apply to an issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueUpdate {
    pub status: Option<String>,
    pub comment: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

/// Event from polling a PM tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PmEvent {
    IssueCreated(IssueSummary),
    IssueUpdated(IssueSummary),
}
