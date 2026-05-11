use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    Beads,
    GitHub,
    Linear,
    Plane,
}

impl std::fmt::Display for PmSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PmSource::Beads => write!(f, "beads"),
            PmSource::GitHub => write!(f, "github"),
            PmSource::Linear => write!(f, "linear"),
            PmSource::Plane => write!(f, "plane"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub status: String,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub status: String,
    pub labels: Vec<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub priority_min: Option<i32>,
    pub priority_max: Option<i32>,
    pub issue_type: Option<String>,
    pub text_search: Option<String>,
    /// Include closed/non-open issues when the backend defaults to open-only.
    pub include_closed: bool,
    /// None = backend default (typically 50)
    pub limit: Option<usize>,
    /// Optional zero-based offset for paginated scans.
    pub offset: Option<usize>,
}

/// Parameters for creating a new issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueCreate {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// task, bug, feature, epic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    /// 0 = critical, 4 = backlog
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Parent issue ID — creates a parent-child dependency (e.g., epic → task).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Time estimate in minutes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate_minutes: Option<u32>,
    /// Issue IDs this new issue depends on (blocking dependencies).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueUpdate {
    pub status: Option<String>,
    pub comment: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub priority: Option<i32>,
    /// Some("alice") = assign, Some("") = unassign, None = no change
    pub assignee: Option<String>,
    /// Some(text) replaces the issue body/description; None leaves it unchanged.
    /// (bd-2m2u Phase 2c — required by `ModifyTaskSpec`.)
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrParams {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PmEvent {
    IssueCreated(IssueSummary),
    IssueUpdated(IssueSummary),
}

#[cfg(test)]
mod tests {
    #[test]
    fn issue_filter_offset_defaults_to_none() {
        let filter = super::IssueFilter::default();
        assert_eq!(filter.offset, None);
        assert_eq!(filter.limit, None);
        assert!(!filter.include_closed);
    }
}
