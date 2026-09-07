use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use spur_acp::DelegationResult;
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

#[derive(Clone, Default)]
struct MockPmState {
    next_issue: u64,
    next_comment: u64,
    issues: HashMap<String, spur_pm::Issue>,
    dependency_edges: Vec<spur_pm::graph::GraphEdge>,
    comments: HashMap<String, Vec<spur_pm::Comment>>,
    atomic_payloads: HashMap<String, String>,
    fail_create_issues_remaining: usize,
}

fn is_blocking_edge(edge_type: Option<&str>) -> bool {
    matches!(
        edge_type,
        Some("blocks" | "conditional-blocks" | "waits-for")
    )
}

fn active_blockers(state: &MockPmState, issue_id: &str) -> Vec<String> {
    let mut blockers = state
        .dependency_edges
        .iter()
        .filter(|edge| edge.to == issue_id && is_blocking_edge(edge.edge_type.as_deref()))
        .filter(|edge| {
            state
                .issues
                .get(&edge.from)
                .is_none_or(|issue| issue.status != "closed")
        })
        .map(|edge| edge.from.clone())
        .collect::<Vec<_>>();
    blockers.sort();
    blockers.dedup();
    blockers
}

fn issue_snapshot(state: &MockPmState, id: &str) -> Option<spur_pm::Issue> {
    let mut issue = state.issues.get(id)?.clone();
    issue.blocked_by = active_blockers(state, id);
    Some(issue)
}

fn dependency_graph(state: &MockPmState) -> spur_pm::graph::DependencyGraph {
    let mut nodes = state
        .issues
        .keys()
        .map(|id| spur_pm::graph::GraphNode {
            id: id.clone(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let mut edges = state.dependency_edges.clone();
    edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
            .then_with(|| left.edge_type.cmp(&right.edge_type))
    });
    spur_pm::graph::DependencyGraph {
        format: Some("json".to_string()),
        adjacency: Some(spur_pm::graph::AdjacencyData {
            nodes,
            edges: Some(edges),
        }),
        ..Default::default()
    }
}

fn apply_issue_update_locked(
    state: &mut MockPmState,
    id: &str,
    update: spur_pm::IssueUpdate,
) -> anyhow::Result<()> {
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
        if let Some(source_system) = update.source_system {
            issue.source_system = source_system;
        }
        if let Some(source_repo) = update.source_repo {
            issue.source_repo = source_repo;
        }
        if let Some(external_ref) = update.external_ref {
            issue.external_ref = external_ref;
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
        add_comment_locked(state, id, comment);
    }
    Ok(())
}

impl MockPm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub async fn issue(&self, id: &str) -> spur_pm::Issue {
        let state = self.inner.lock().await;
        issue_snapshot(&state, id).unwrap_or_else(|| panic!("missing mock issue {id}"))
    }

    pub async fn issues(&self) -> Vec<spur_pm::Issue> {
        let state = self.inner.lock().await;
        let mut issues = state
            .issues
            .keys()
            .filter_map(|id| issue_snapshot(&state, id))
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
        let state = self.inner.lock().await;
        issue_snapshot(&state, id).with_context(|| format!("mock issue not found: {id}"))
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
        let dependencies = params.depends_on.clone();
        let parent = params.parent.clone();
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
            blocked_by: Vec::new(),
            due_at: None,
            created_at: now,
            updated_at: now,
            external_ref: None,
            source_system: None,
            source_repo: None,
        };
        state.issues.insert(id.clone(), issue);
        if let Some(parent) = parent {
            state.dependency_edges.push(spur_pm::graph::GraphEdge {
                from: parent,
                to: id.clone(),
                edge_type: Some("parent-child".to_string()),
            });
        }
        for dependency in dependencies {
            if !state.dependency_edges.iter().any(|edge| {
                edge.from == dependency
                    && edge.to == id
                    && edge.edge_type.as_deref() == Some("blocks")
            }) {
                state.dependency_edges.push(spur_pm::graph::GraphEdge {
                    from: dependency,
                    to: id.clone(),
                    edge_type: Some("blocks".to_string()),
                });
            }
        }
        Ok(id)
    }

    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        let mut state = self.inner.lock().await;
        apply_issue_update_locked(&mut state, id, update)
    }

    async fn update_issues_atomically(
        &self,
        idempotency_key: &str,
        preconditions: Vec<spur_pm::AtomicUpdatePrecondition>,
        updates: Vec<(String, spur_pm::IssueUpdate)>,
    ) -> anyhow::Result<spur_pm::AtomicUpdateOutcome> {
        let payload = serde_json::to_string(&updates)?;
        let mut state = self.inner.lock().await;
        if let Some(existing) = state.atomic_payloads.get(idempotency_key) {
            anyhow::ensure!(
                existing == &payload,
                "mock atomic update key reused with a different payload"
            );
            return Ok(spur_pm::AtomicUpdateOutcome::AlreadyApplied);
        }

        for precondition in &preconditions {
            let actual = state
                .comments
                .get(&precondition.issue_id)
                .map_or(0, Vec::len) as u64;
            anyhow::ensure!(
                actual == precondition.expected_comment_count,
                "mock atomic precondition failed for '{}': expected {} comments, found {}",
                precondition.issue_id,
                precondition.expected_comment_count,
                actual
            );
        }

        let mut candidate = state.clone();
        for (issue_id, update) in updates {
            apply_issue_update_locked(&mut candidate, &issue_id, update)?;
        }
        candidate
            .atomic_payloads
            .insert(idempotency_key.to_string(), payload);
        *state = candidate;
        Ok(spur_pm::AtomicUpdateOutcome::Applied)
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        let mut state = self.inner.lock().await;
        anyhow::ensure!(
            state.issues.contains_key(issue_id),
            "mock issue not found: {issue_id}"
        );
        if !state.dependency_edges.iter().any(|edge| {
            edge.from == depends_on_id
                && edge.to == issue_id
                && edge.edge_type.as_deref() == Some("blocks")
        }) {
            state.dependency_edges.push(spur_pm::graph::GraphEdge {
                from: depends_on_id.to_string(),
                to: issue_id.to_string(),
                edge_type: Some("blocks".to_string()),
            });
        }
        state
            .issues
            .get_mut(issue_id)
            .expect("mock issue existence checked")
            .updated_at = Utc::now();
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

    fn issue_graph_available(&self) -> bool {
        true
    }

    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph> {
        let state = self.inner.lock().await;
        anyhow::ensure!(state.issues.contains_key(id), "mock issue not found: {id}");
        // Return the complete finite fixture. Consumers still apply their own
        // root, membership, direction, and edge-type filters.
        Ok(dependency_graph(&state))
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
                    || active_blockers(&state, &issue.id).is_empty()
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
        anyhow::ensure!(
            state.issues.contains_key(issue_id),
            "mock issue not found: {issue_id}"
        );
        state.dependency_edges.retain(|edge| {
            !(edge.from == depends_on_id
                && edge.to == issue_id
                && is_blocking_edge(edge.edge_type.as_deref()))
        });
        state
            .issues
            .get_mut(issue_id)
            .expect("mock issue existence checked")
            .updated_at = Utc::now();
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

pub fn mock_worker_completion(
    request: crate::DelegationRequest,
    result: DelegationResult,
    base_oid: &str,
) -> anyhow::Result<()> {
    if let Some(tx) = &request.dispatched_base_oid_tx {
        tx.send(Some(base_oid.to_string()))
            .map_err(|error| anyhow::anyhow!("send dispatched_base_oid: {error}"))?;
    }
    request
        .respond_to
        .send(result)
        .map_err(|error| anyhow::anyhow!("send delegation result: {error:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MockPm;
    use crate::plan::PmLike;

    #[tokio::test]
    async fn mock_pm_keeps_parent_and_external_dependency_edges_distinct() {
        let pm = MockPm::new();
        let epic = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Epic".to_string(),
                issue_type: Some("epic".to_string()),
                ..Default::default()
            })
            .await
            .expect("create epic");
        let external = pm
            .create_issue(spur_pm::IssueCreate {
                title: "External prerequisite".to_string(),
                issue_type: Some("task".to_string()),
                ..Default::default()
            })
            .await
            .expect("create external prerequisite");
        let child = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Child".to_string(),
                issue_type: Some("task".to_string()),
                parent: Some(epic.clone()),
                depends_on: vec![external.clone()],
                ..Default::default()
            })
            .await
            .expect("create child");

        assert_eq!(
            pm.issue(&child).await.blocked_by,
            vec![external.clone()],
            "parent ownership must not leak into the active blocker view"
        );

        let graph = pm
            .issue_subgraph_json(&epic)
            .await
            .expect("mock structural graph");
        let edges = graph
            .adjacency
            .expect("mock graph adjacency")
            .edges
            .expect("mock graph edges");
        assert!(edges.iter().any(|edge| {
            edge.from == epic
                && edge.to == child
                && edge.edge_type.as_deref() == Some("parent-child")
        }));
        assert!(edges.iter().any(|edge| {
            edge.from == external && edge.to == child && edge.edge_type.as_deref() == Some("blocks")
        }));

        pm.update_issue(
            &external,
            spur_pm::IssueUpdate {
                status: Some("closed".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close external prerequisite");
        assert!(
            pm.issue(&child).await.blocked_by.is_empty(),
            "closed prerequisites must leave the active blocker view"
        );

        let closed_graph = pm
            .issue_subgraph_json(&epic)
            .await
            .expect("mock structural graph after close");
        assert!(
            closed_graph
                .adjacency
                .expect("mock graph adjacency")
                .edges
                .expect("mock graph edges")
                .iter()
                .any(|edge| {
                    edge.from == external
                        && edge.to == child
                        && edge.edge_type.as_deref() == Some("blocks")
                }),
            "closed prerequisite history must remain structural"
        );

        spur_pm::BeadsAdvanced::remove_dependency(&pm, &child, &external)
            .await
            .expect("remove external prerequisite");
        let removed_graph = pm
            .issue_subgraph_json(&epic)
            .await
            .expect("mock structural graph after remove");
        assert!(!removed_graph
            .adjacency
            .expect("mock graph adjacency")
            .edges
            .expect("mock graph edges")
            .iter()
            .any(|edge| {
                edge.from == external
                    && edge.to == child
                    && edge.edge_type.as_deref() == Some("blocks")
            }));

        pm.add_dependency(&child, &external)
            .await
            .expect("re-add closed prerequisite");
        assert!(
            pm.issue(&child).await.blocked_by.is_empty(),
            "re-adding a closed prerequisite must not reactivate it"
        );
        let readded_graph = pm
            .issue_subgraph_json(&epic)
            .await
            .expect("mock structural graph after re-add");
        assert!(readded_graph
            .adjacency
            .expect("mock graph adjacency")
            .edges
            .expect("mock graph edges")
            .iter()
            .any(|edge| {
                edge.from == external
                    && edge.to == child
                    && edge.edge_type.as_deref() == Some("blocks")
            }));
    }

    #[tokio::test]
    async fn mock_pm_external_dependency_graph_tracks_add_and_remove() {
        let pm = MockPm::new();
        let prerequisite = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Prerequisite".to_string(),
                issue_type: Some("task".to_string()),
                ..Default::default()
            })
            .await
            .expect("create prerequisite");
        let dependent = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Dependent".to_string(),
                issue_type: Some("task".to_string()),
                ..Default::default()
            })
            .await
            .expect("create dependent");

        pm.add_dependency(&dependent, &prerequisite)
            .await
            .expect("add dependency");
        let added_graph = pm
            .issue_subgraph_json(&dependent)
            .await
            .expect("mock graph after add");
        assert!(added_graph
            .adjacency
            .expect("mock graph adjacency")
            .edges
            .expect("mock graph edges")
            .iter()
            .any(|edge| {
                edge.from == prerequisite
                    && edge.to == dependent
                    && edge.edge_type.as_deref() == Some("blocks")
            }));

        spur_pm::BeadsAdvanced::remove_dependency(&pm, &dependent, &prerequisite)
            .await
            .expect("remove dependency");
        let removed_graph = pm
            .issue_subgraph_json(&dependent)
            .await
            .expect("mock graph after remove");
        assert!(!removed_graph
            .adjacency
            .expect("mock graph adjacency")
            .edges
            .expect("mock graph edges")
            .iter()
            .any(|edge| edge.from == prerequisite && edge.to == dependent));
    }
}
