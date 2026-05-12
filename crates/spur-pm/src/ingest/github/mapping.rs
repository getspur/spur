//! Pure mapping functions (spec §7.4). No I/O. The translator from a
//! GitHub GraphQL node to either a fresh `IssueCreate` or a diff against
//! an existing local `Issue` lives here so it can be unit-tested with
//! fixtures and reused by the apply step (PR-3) and the conflict
//! detector.
//!
//! Three entry points:
//!
//!   - [`to_remote_node`] — `IssueNode | PrNode` → `RemoteNode`. Source-of-
//!     truth conversion; everything else builds on top.
//!   - [`to_issue_create`] — `RemoteNode` + `IngestOptions` → `IssueCreate`.
//!     Used when no local issue with this `external_ref` exists yet.
//!   - [`diff_against_local`] — `&Issue` + `&RemoteNode` → `MappedDiff`.
//!     Returns the field-level changes the apply step should write back.
//!     Exposes [`MappedDiff::remote_changed_fields`] so PR-3's conflict
//!     detector can ask "which mapped fields did the remote change?"
//!     without re-deriving the diff.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::sync::{RemoteComment, RemoteKind, RemoteNode, RemoteState};
use crate::types::{Issue, IssueCreate, IssueUpdate};

use super::graphql::{IssueNode, PrNode};

/// Per-call ingest options. Only `label_namespace` is used today; the
/// struct is intentionally generous so PR-3 can pass through CLI flags
/// without a signature churn.
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Default `gh`. Forms the prefix for synthesized labels:
    /// `<ns>:issue`, `<ns>:pull-request`, `<ns>:<remote-label-name>`,
    /// `<ns>:also-assigned:<login>`, etc.
    pub label_namespace: String,
    /// Source repo, e.g. "getspur/spur". Persisted as `source_repo`.
    pub source_repo: String,
}

impl IngestOptions {
    pub fn new(source_repo: impl Into<String>) -> Self {
        Self {
            label_namespace: "gh".to_string(),
            source_repo: source_repo.into(),
        }
    }
}

/// Bitset of mapped fields. Cheap to compose; the conflict detector in PR-3
/// intersects this with the local-changed bitset to decide "both moved".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSet {
    pub title: bool,
    pub body: bool,
    pub status: bool,
    pub labels: bool,
    pub assignee: bool,
}

impl FieldSet {
    pub fn is_empty(&self) -> bool {
        !(self.title || self.body || self.status || self.labels || self.assignee)
    }
}

/// Output of `diff_against_local`. Carries both the `IssueUpdate` the apply
/// step should write *and* a [`FieldSet`] enumerating which mapped fields
/// the remote moved since the watermark.
#[derive(Debug, Clone)]
pub struct MappedDiff {
    pub update: IssueUpdate,
    /// Comments observed on the remote that are *new* compared to the local
    /// imported set. (Caller still has to dedupe via §4.2 marker scan.)
    pub new_comments: Vec<RemoteComment>,
    changed: FieldSet,
}

impl MappedDiff {
    /// Used by PR-3's conflict detector. Read-only access keeps the field
    /// set authoritative inside this module.
    pub fn remote_changed_fields(&self) -> FieldSet {
        self.changed
    }

    pub fn is_noop(&self) -> bool {
        self.changed.is_empty() && self.new_comments.is_empty()
    }
}

// -----------------------------------------------------------------------
// Entry points
// -----------------------------------------------------------------------

pub fn issue_node_to_remote(node: &IssueNode) -> RemoteNode {
    let state = match (node.state.as_str(), node.state_reason.as_deref()) {
        ("OPEN", _) => RemoteState::Open,
        ("CLOSED", reason) => RemoteState::Closed {
            reason: reason.map(|s| s.to_string()),
        },
        (other, _) => RemoteState::Closed {
            reason: Some(other.to_string()),
        },
    };

    RemoteNode {
        remote_id: node.id.clone(),
        remote_number: Some(node.number),
        kind: RemoteKind::Issue,
        title: node.title.clone(),
        body: node.body.clone(),
        state,
        labels: node.labels.nodes.iter().map(|l| l.name.clone()).collect(),
        assignees: node
            .assignees
            .nodes
            .iter()
            .map(|a| a.login.clone())
            .collect(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        html_url: node.url.clone(),
        etag: None,
        dep_hints: Vec::new(), // populated by dep_hints.rs in PR-3
        comments: node
            .comments
            .nodes
            .iter()
            .map(graphql_comment_to_remote)
            .collect(),
        raw: serde_json::Value::Null,
    }
}

pub fn pr_node_to_remote(node: &PrNode) -> RemoteNode {
    let state = match (node.state.as_str(), node.is_draft) {
        ("OPEN", true) => RemoteState::Draft,
        ("OPEN", false) => RemoteState::Open,
        ("MERGED", _) => RemoteState::Closed {
            reason: Some("MERGED".to_string()),
        },
        ("CLOSED", _) => RemoteState::Closed { reason: None },
        (other, _) => RemoteState::Closed {
            reason: Some(other.to_string()),
        },
    };

    RemoteNode {
        remote_id: node.id.clone(),
        remote_number: Some(node.number),
        kind: RemoteKind::PullRequest,
        title: node.title.clone(),
        body: node.body.clone(),
        state,
        labels: node.labels.nodes.iter().map(|l| l.name.clone()).collect(),
        assignees: node
            .assignees
            .nodes
            .iter()
            .map(|a| a.login.clone())
            .collect(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        html_url: node.url.clone(),
        etag: None,
        dep_hints: Vec::new(),
        comments: node
            .comments
            .nodes
            .iter()
            .map(graphql_comment_to_remote)
            .collect(),
        raw: serde_json::Value::Null,
    }
}

fn graphql_comment_to_remote(c: &super::graphql::CommentNode) -> RemoteComment {
    RemoteComment {
        remote_id: c.id.clone(),
        author: c
            .author
            .as_ref()
            .map(|a| a.login.clone())
            .unwrap_or_default(),
        body: c.body.clone(),
        created_at: c.created_at,
        updated_at: c.updated_at,
    }
}

/// Synthesize an `IssueCreate` from a remote node and ingest options.
/// Mapping table per spec §7.4. `assignee` takes the first remote assignee;
/// additional assignees become `<ns>:also-assigned:<login>` labels (Phase 1
/// log+drop semantics live in the apply step, not here).
pub fn to_issue_create(remote: &RemoteNode, opts: &IngestOptions) -> IssueCreate {
    let mut labels = Vec::with_capacity(remote.labels.len() + 4);
    let kind_label = match remote.kind {
        RemoteKind::Issue => format!("{}:issue", opts.label_namespace),
        RemoteKind::PullRequest => format!("{}:pull-request", opts.label_namespace),
    };
    labels.push(kind_label);

    for raw in &remote.labels {
        labels.push(format!("{}:{}", opts.label_namespace, raw));
    }

    if let RemoteState::Closed { reason: Some(r) } = &remote.state {
        match r.as_str() {
            "NOT_PLANNED" => labels.push(format!("{}:not-planned", opts.label_namespace)),
            "MERGED" => labels.push(format!("{}:merged", opts.label_namespace)),
            _ => {}
        }
    }

    // Multi-assignee log+drop: synthesize labels for non-primary assignees.
    for extra in remote.assignees.iter().skip(1) {
        labels.push(format!("{}:also-assigned:{extra}", opts.label_namespace));
    }

    let issue_type = infer_issue_type(&remote.labels, remote.kind);
    let priority = infer_priority(&remote.labels);

    IssueCreate {
        title: remote.title.clone(),
        description: Some(remote.body.clone()),
        issue_type,
        priority,
        labels,
        parent: None,
        assignee: remote
            .assignees
            .first()
            .map(|login| format!("{}:{login}", opts.label_namespace)),
        estimate_minutes: None,
        depends_on: Vec::new(),
        source_system: Some("github".to_string()),
        source_repo: Some(opts.source_repo.clone()),
        external_ref: Some(format!("github:{}", remote.remote_id)),
    }
}

/// Field-level diff between a local `Issue` and a freshly-fetched
/// `RemoteNode`. Only fields that differ are emitted; this keeps
/// beads_rust's audit history clean (spec §7.4 last paragraph).
pub fn diff_against_local(local: &Issue, remote: &RemoteNode, opts: &IngestOptions) -> MappedDiff {
    let mut update = IssueUpdate::default();
    let mut changed = FieldSet::default();

    // title
    if local.title != remote.title {
        // IssueUpdate has no title field today; spec §7.4 stages this for
        // the dedicated title-update path PR-3 will add. Track the change
        // in the field set so the conflict detector still sees it.
        changed.title = true;
    }

    // body / description
    if local.body != remote.body {
        update.body = Some(remote.body.clone());
        changed.body = true;
    }

    // status
    let mapped_status = remote_status_string(&remote.state);
    if local.status != mapped_status {
        update.status = Some(mapped_status);
        changed.status = true;
    }

    // labels — set diff of namespaced names
    let want_labels = synthesize_labels(remote, opts);
    let have: std::collections::BTreeSet<&str> = local.labels.iter().map(|s| s.as_str()).collect();
    let want: std::collections::BTreeSet<&str> = want_labels.iter().map(|s| s.as_str()).collect();
    let to_add: Vec<String> = want.difference(&have).map(|s| s.to_string()).collect();
    let to_remove: Vec<String> = have
        .difference(&want)
        .filter(|name| name.starts_with(&format!("{}:", opts.label_namespace)))
        .map(|s| s.to_string())
        .collect();
    if !to_add.is_empty() || !to_remove.is_empty() {
        update.add_labels = to_add;
        update.remove_labels = to_remove;
        changed.labels = true;
    }

    // assignee
    let want_assignee = remote
        .assignees
        .first()
        .map(|login| format!("{}:{login}", opts.label_namespace));
    if local.assignee != want_assignee {
        update.assignee = Some(want_assignee.unwrap_or_default());
        changed.assignee = true;
    }

    MappedDiff {
        update,
        new_comments: remote.comments.clone(),
        changed,
    }
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn synthesize_labels(remote: &RemoteNode, opts: &IngestOptions) -> Vec<String> {
    let create = to_issue_create(remote, opts);
    create.labels
}

fn remote_status_string(state: &RemoteState) -> String {
    match state {
        RemoteState::Open => "open".to_string(),
        RemoteState::Draft => "draft".to_string(),
        RemoteState::Closed { reason } => match reason.as_deref() {
            Some("REOPENED") => "open".to_string(),
            _ => "closed".to_string(),
        },
    }
}

fn infer_issue_type(remote_labels: &[String], kind: RemoteKind) -> Option<String> {
    for label in remote_labels {
        let l = label.to_lowercase();
        if l == "bug" {
            return Some("bug".to_string());
        }
        if l == "enhancement" || l == "feature" {
            return Some("feature".to_string());
        }
        if l == "documentation" || l == "docs" {
            return Some("docs".to_string());
        }
        if l == "question" {
            return Some("question".to_string());
        }
        if l == "chore" {
            return Some("chore".to_string());
        }
    }
    Some(match kind {
        RemoteKind::PullRequest => "feature".to_string(),
        RemoteKind::Issue => "task".to_string(),
    })
}

fn infer_priority(remote_labels: &[String]) -> Option<i32> {
    let re = regex::Regex::new(r"(?i)^p([0-4])$|^priority[-/:]p?([0-4])").ok()?;
    for label in remote_labels {
        if let Some(caps) = re.captures(label) {
            let digit = caps
                .get(1)
                .or_else(|| caps.get(2))
                .and_then(|m| m.as_str().parse::<i32>().ok());
            if let Some(p) = digit {
                return Some(p);
            }
        }
    }
    None
}

// Convenience: serialise a RemoteNode into a stable JSON blob for the
// `raw` slot. Useful for fixtures and round-trip tests.
pub fn remote_node_raw(node: &RemoteNode) -> serde_json::Value {
    json!({
        "remote_id": node.remote_id,
        "remote_number": node.remote_number,
        "kind": format!("{:?}", node.kind),
        "labels": node.labels,
        "captured_at": Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::RemoteKind;

    fn sample_remote_issue() -> RemoteNode {
        RemoteNode {
            remote_id: "I_kwDO_test_001".to_string(),
            remote_number: Some(42),
            kind: RemoteKind::Issue,
            title: "Crash on startup".to_string(),
            body: "Steps to reproduce…".to_string(),
            state: RemoteState::Open,
            labels: vec!["bug".to_string(), "p1".to_string()],
            assignees: vec!["alice".to_string(), "bob".to_string()],
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339("2025-01-02T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            html_url: "https://github.com/getspur/spur/issues/42".to_string(),
            etag: None,
            dep_hints: Vec::new(),
            comments: Vec::new(),
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn issue_create_sets_provenance_and_labels() {
        let opts = IngestOptions::new("getspur/spur");
        let create = to_issue_create(&sample_remote_issue(), &opts);

        assert_eq!(create.title, "Crash on startup");
        assert_eq!(create.source_system.as_deref(), Some("github"));
        assert_eq!(create.source_repo.as_deref(), Some("getspur/spur"));
        assert_eq!(
            create.external_ref.as_deref(),
            Some("github:I_kwDO_test_001")
        );
        assert_eq!(create.assignee.as_deref(), Some("gh:alice"));
        assert_eq!(create.issue_type.as_deref(), Some("bug"));
        assert_eq!(create.priority, Some(1));
        assert!(create.labels.contains(&"gh:issue".to_string()));
        assert!(create.labels.contains(&"gh:bug".to_string()));
        assert!(create.labels.contains(&"gh:p1".to_string()));
        assert!(create.labels.contains(&"gh:also-assigned:bob".to_string()));
    }

    #[test]
    fn pr_state_maps_merged_to_closed_with_label() {
        let pr = RemoteNode {
            kind: RemoteKind::PullRequest,
            state: RemoteState::Closed {
                reason: Some("MERGED".to_string()),
            },
            ..sample_remote_issue()
        };
        let opts = IngestOptions::new("getspur/spur");
        let create = to_issue_create(&pr, &opts);
        assert!(create.labels.contains(&"gh:pull-request".to_string()));
        assert!(create.labels.contains(&"gh:merged".to_string()));
    }

    #[test]
    fn closed_not_planned_emits_label() {
        let issue = RemoteNode {
            state: RemoteState::Closed {
                reason: Some("NOT_PLANNED".to_string()),
            },
            ..sample_remote_issue()
        };
        let create = to_issue_create(&issue, &IngestOptions::new("getspur/spur"));
        assert!(create.labels.contains(&"gh:not-planned".to_string()));
    }

    #[test]
    fn diff_no_change_is_noop() {
        let opts = IngestOptions::new("getspur/spur");
        let remote = sample_remote_issue();
        let local = create_to_issue(&to_issue_create(&remote, &opts), &remote);
        let diff = diff_against_local(&local, &remote, &opts);
        assert!(
            diff.is_noop(),
            "fresh local matched against same remote should be a no-op; got {:?}",
            diff.changed
        );
    }

    #[test]
    fn diff_title_change_marks_field_set() {
        let opts = IngestOptions::new("getspur/spur");
        let remote = sample_remote_issue();
        let mut local = create_to_issue(&to_issue_create(&remote, &opts), &remote);
        local.title = "Different title".to_string();
        let diff = diff_against_local(&local, &remote, &opts);
        assert!(
            diff.changed.title,
            "title flag must be set when titles diverge"
        );
        // changed_fields() is what PR-3's conflict detector calls
        assert!(diff.remote_changed_fields().title);
    }

    #[test]
    fn diff_body_change_emits_update_and_flag() {
        let opts = IngestOptions::new("getspur/spur");
        let remote = sample_remote_issue();
        let mut local = create_to_issue(&to_issue_create(&remote, &opts), &remote);
        local.body = "stale body".to_string();
        let diff = diff_against_local(&local, &remote, &opts);
        assert!(diff.changed.body);
        assert_eq!(diff.update.body.as_deref(), Some("Steps to reproduce…"));
    }

    /// Reconstruct a local Issue from an IssueCreate so we can write
    /// "matches itself" assertions. Mirrors what BeadsCrateAdapter::create
    /// produces in spirit (only the fields the diff cares about).
    fn create_to_issue(c: &IssueCreate, remote: &RemoteNode) -> Issue {
        Issue {
            id: "bd-test".to_string(),
            source: crate::types::PmSource::Beads,
            title: c.title.clone(),
            body: c.description.clone().unwrap_or_default(),
            status: remote_status_string(&remote.state),
            labels: c.labels.clone(),
            assignee: c.assignee.clone(),
            url: remote.html_url.clone(),
            priority: c.priority,
            issue_type: c.issue_type.clone(),
            blocked_by: Vec::new(),
            due_at: None,
            source_system: c.source_system.clone(),
            source_repo: c.source_repo.clone(),
            external_ref: c.external_ref.clone(),
            created_at: remote.created_at,
            updated_at: remote.updated_at,
        }
    }
}
