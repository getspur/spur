use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Mutex;

/// In-memory beads-compatible PM fixture for plan/substrate tests.
///
/// API surface:
/// - [`MockPm::new`] creates an empty concurrent fixture.
/// - Clone the value or call [`MockPm::arc`] to share it across server tasks.
/// - Install it into `McpCallbackServer` tests via `__test_set_pm_like`.
/// - Use [`MockPm::issue`], [`MockPm::comments`], and [`MockPm::issues`] for
///   assertions against created epics, children, labels, comments, and
///   dependency edges.
/// - [`MockPm::audit_seq`] returns a monotonic sequence derived from total
///   stored comment count. This is useful for PR5 submit/truncate tests; PR3
///   version-cache tests must use a per-epic counter instead.
///
/// The fixture is test-only and intentionally lives in `spur-mcp`; production
/// code continues to use `spur_pm::PmService`.
#[derive(Clone, Default)]
pub struct MockPm {
    inner: Arc<Mutex<MockPmState>>,
}

#[derive(Default)]
struct MockPmState {
    next_issue: u64,
    next_comment: u64,
    issues: HashMap<String, spur_pm::Issue>,
    comments: HashMap<String, Vec<spur_pm::Comment>>,
    fail_create_issues_remaining: usize,
}

impl MockPm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub async fn issue(&self, id: &str) -> spur_pm::Issue {
        self.inner
            .lock()
            .await
            .issues
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("missing mock issue {id}"))
    }

    pub async fn issues(&self) -> Vec<spur_pm::Issue> {
        let mut issues = self
            .inner
            .lock()
            .await
            .issues
            .values()
            .cloned()
            .collect::<Vec<_>>();
        issues.sort_by(|left, right| left.id.cmp(&right.id));
        issues
    }

    pub async fn comments(&self, issue_id: &str) -> Vec<spur_pm::Comment> {
        self.inner
            .lock()
            .await
            .comments
            .get(issue_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn audit_seq(&self) -> u64 {
        // PR3-NOTE: this returns SUM across all issues, NOT per-epic count.
        // Production BeadsVersion::AuditSeq (server.rs:~6920) counts audit
        // sentinels on the epic issue ONLY. PR3's versioned-cache tests must
        // either override this or scope it to a specific epic_id.
        self.inner
            .lock()
            .await
            .comments
            .values()
            .map(Vec::len)
            .sum::<usize>() as u64
    }

    pub async fn fail_next_create_issues(&self, count: usize) {
        self.inner.lock().await.fail_create_issues_remaining = count;
    }
}

#[async_trait]
impl crate::plan::PmLike for MockPm {
    async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
        self.inner
            .lock()
            .await
            .issues
            .get(id)
            .cloned()
            .with_context(|| format!("mock issue not found: {id}"))
    }

    async fn list_issues(
        &self,
        filter: spur_pm::IssueFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        let state = self.inner.lock().await;
        let mut summaries = state
            .issues
            .values()
            .filter(|issue| {
                if filter.status.is_none() && !filter.include_closed && issue.status != "open" {
                    return false;
                }
                if let Some(status) = filter.status.as_deref() {
                    if issue.status != status {
                        return false;
                    }
                }
                if let Some(issue_type) = filter.issue_type.as_deref() {
                    if issue.issue_type.as_deref() != Some(issue_type) {
                        return false;
                    }
                }
                if let Some(assignee) = filter.assignee.as_deref() {
                    if issue.assignee.as_deref() != Some(assignee) {
                        return false;
                    }
                }
                if !filter
                    .labels
                    .iter()
                    .all(|label| issue.labels.contains(label))
                {
                    return false;
                }
                true
            })
            .map(issue_summary)
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(offset) = filter.offset {
            summaries = summaries.into_iter().skip(offset).collect();
        }
        if let Some(limit) = filter.limit {
            summaries.truncate(limit);
        }
        Ok(summaries)
    }

    async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
        let mut state = self.inner.lock().await;
        if state.fail_create_issues_remaining > 0 {
            state.fail_create_issues_remaining -= 1;
            anyhow::bail!("mock create_issue failure");
        }
        state.next_issue += 1;
        let id = format!("bd-mock-{}", state.next_issue);
        let now = Utc::now();
        let mut blocked_by = params.depends_on;
        if let Some(parent) = params.parent {
            blocked_by.push(parent);
        }
        let mut labels = dedupe(params.labels);
        labels.sort();
        let issue = spur_pm::Issue {
            id: id.clone(),
            source: spur_pm::PmSource::Beads,
            title: params.title,
            body: params.description.unwrap_or_default(),
            status: "open".to_string(),
            labels,
            assignee: params.assignee,
            url: format!("mock://{id}"),
            priority: params.priority,
            issue_type: params.issue_type,
            blocked_by,
            due_at: None,
            created_at: now,
            updated_at: now,
            external_ref: None,
            source_system: None,
            source_repo: None,
        };
        state.issues.insert(id.clone(), issue);
        Ok(id)
    }

    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        let mut state = self.inner.lock().await;
        {
            let issue = state
                .issues
                .get_mut(id)
                .with_context(|| format!("mock issue not found: {id}"))?;
            if let Some(status) = update.status {
                issue.status = status;
            }
            if let Some(body) = update.body {
                issue.body = body;
            }
            if let Some(priority) = update.priority {
                issue.priority = Some(priority);
            }
            if let Some(assignee) = update.assignee {
                issue.assignee = if assignee.is_empty() {
                    None
                } else {
                    Some(assignee)
                };
            }
            if !update.remove_labels.is_empty() {
                let remove = update.remove_labels.into_iter().collect::<HashSet<_>>();
                issue.labels.retain(|label| !remove.contains(label));
            }
            for label in update.add_labels {
                if !issue.labels.contains(&label) {
                    issue.labels.push(label);
                }
            }
            issue.labels.sort();
            issue.updated_at = Utc::now();
        }
        if let Some(comment) = update.comment {
            add_comment_locked(&mut state, id, comment);
        }
        Ok(())
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        let mut state = self.inner.lock().await;
        let issue = state
            .issues
            .get_mut(issue_id)
            .with_context(|| format!("mock issue not found: {issue_id}"))?;
        if !issue.blocked_by.iter().any(|id| id == depends_on_id) {
            issue.blocked_by.push(depends_on_id.to_string());
        }
        issue.updated_at = Utc::now();
        Ok(())
    }

    async fn issue_labels(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.get_issue(id).await?.labels)
    }

    fn closed_status(&self) -> &str {
        "closed"
    }

    fn source_str(&self) -> &'static str {
        "beads"
    }

    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        Some(self)
    }
}

#[async_trait]
impl spur_pm::BeadsAdvanced for MockPm {
    async fn list_ready(
        &self,
        filter: spur_pm::ReadyFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        let state = self.inner.lock().await;
        let mut summaries = state
            .issues
            .values()
            .filter(|issue| issue.status == "open")
            .filter(|issue| {
                filter
                    .assignee
                    .as_deref()
                    .is_none_or(|assignee| issue.assignee.as_deref() == Some(assignee))
            })
            .filter(|issue| {
                filter
                    .issue_type
                    .as_deref()
                    .is_none_or(|issue_type| issue.issue_type.as_deref() == Some(issue_type))
            })
            .filter(|issue| {
                filter
                    .labels_all
                    .iter()
                    .all(|label| issue.labels.contains(label))
            })
            .filter(|issue| {
                filter.labels_any.is_empty()
                    || filter
                        .labels_any
                        .iter()
                        .any(|label| issue.labels.contains(label))
            })
            .filter(|issue| {
                filter.priorities.is_empty()
                    || issue
                        .priority
                        .is_some_and(|priority| filter.priorities.contains(&priority))
            })
            .filter(|issue| {
                issue.issue_type.as_deref() == Some("epic")
                    || issue.blocked_by.iter().all(|blocker| {
                        state.issues.get(blocker).is_none_or(|blocked_by| {
                            blocked_by.issue_type.as_deref() == Some("epic")
                                || blocked_by.status != "open"
                        })
                    })
            })
            .map(issue_summary)
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(limit) = filter.limit {
            summaries.truncate(limit);
        }
        Ok(summaries)
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
        Ok(self.comments(issue_id).await)
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<spur_pm::CommentId> {
        let mut state = self.inner.lock().await;
        if !state.issues.contains_key(issue_id) {
            anyhow::bail!("mock issue not found: {issue_id}");
        }
        Ok(add_comment_locked(&mut state, issue_id, body.to_string()))
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        let mut state = self.inner.lock().await;
        let issue = state
            .issues
            .get_mut(issue_id)
            .with_context(|| format!("mock issue not found: {issue_id}"))?;
        issue.blocked_by.retain(|id| id != depends_on_id);
        issue.updated_at = Utc::now();
        Ok(())
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
        Ok(Vec::new())
    }
}

fn add_comment_locked(state: &mut MockPmState, issue_id: &str, body: String) -> String {
    state.next_comment += 1;
    let id = state.next_comment.to_string();
    state
        .comments
        .entry(issue_id.to_string())
        .or_default()
        .push(spur_pm::Comment {
            id: id.clone(),
            body,
            actor: "mock".to_string(),
            created_at: Utc::now(),
        });
    if let Some(issue) = state.issues.get_mut(issue_id) {
        issue.updated_at = Utc::now();
    }
    id
}

fn issue_summary(issue: &spur_pm::Issue) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: issue.id.clone(),
        source: issue.source.clone(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        url: issue.url.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
        description: Some(issue.body.clone()).filter(|b| !b.trim().is_empty()),
    }
}

fn dedupe(labels: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    labels
        .into_iter()
        .filter(|label| seen.insert(label.clone()))
        .collect()
}
