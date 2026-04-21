//! Plan executor — deterministic DAG-based task scheduling.
//!
//! The brain submits a structured plan via `submit_plan`. The executor
//! dispatches tasks to workers in dependency order: tasks with satisfied
//! deps run in parallel, blocked tasks wait. Individual delegations flow
//! through the existing `DelegationRequest` → orchestrator pipeline.

pub mod audit_sentinel;
pub mod labels;
pub mod mutation;
pub mod mutation_executor;
pub mod proposers;
pub mod reconciler;
pub mod signal_watcher;
pub mod signals;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus};

use crate::tools::DelegationRequest;

// ─── Types ───────────────────────────────────────────────────────────

/// A single task in a submitted plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanTask {
    pub task_id: String,
    pub agent: String,
    pub task: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub context_files: Vec<String>,
}

/// Status of an individual plan task.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanTaskStatus {
    /// Waiting for dependencies to complete.
    Pending,
    /// All deps satisfied; about to be dispatched.
    Ready,
    /// Sent to a worker agent.
    Dispatched { delegation_id: String },
    /// Worker completed; awaiting brain review.
    AwaitingReview { summary: Option<String> },
    /// Brain approved the work.
    Approved { summary: Option<String> },
    /// Brain rejected the work.
    Rejected { feedback: Option<String> },
    /// Worker failed or dependency failed.
    Failed { error: String },
    /// Task was cancelled (e.g. by brain or system)
    Cancelled { reason: String },
    /// Task was superseded by a mutation (v0b). `by` lists the child task IDs
    /// that replace this task in the plan graph. Lineage preserved for future
    /// MCTS reward backprop.
    Superseded {
        mutation_id: String,
        by: Vec<String>,
    },
}

impl PlanTaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Approved { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::Superseded { .. }
        )
    }
}

/// Record of a single attempt at a plan task. Stored in `PlanTaskEntry.history`
/// for attempts 1..attempt-1. The current (latest) attempt lives in the entry's
/// top-level `result` and `worker_branch` fields.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub worker_branch: Option<String>,
    pub diff_summary: Option<spur_acp::DiffSummary>,
    pub summary: Option<String>,
    /// Brain's `request_changes` feedback that caused this attempt to be superseded.
    pub feedback: String,
}

/// A task entry in the plan state (spec + runtime status).
#[derive(Debug, Clone, Serialize)]
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    /// Latest attempt's full delegation result. None while Dispatched.
    pub result: Option<DelegationResult>,
    /// Latest attempt's worker branch (preserved in git when set).
    pub worker_branch: Option<String>,
    /// Current attempt number — starts at 1 on initial dispatch.
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// Prior attempts (1..attempt-1). Empty for first-iteration tasks.
    #[serde(default)]
    pub history: Vec<AttemptRecord>,
    /// The delegation_id of the most recently dispatched attempt for this task.
    /// Set when status transitions to Dispatched; retained through AwaitingReview
    /// so audit sentinels (Approval, Rejection) can reference it.
    #[serde(default)]
    pub last_delegation_id: Option<String>,
}

/// Result of attempting to integrate a fully approved plan onto a dedicated
/// plan branch.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanMergeState {
    #[default]
    NotStarted,
    Succeeded {
        merge_branch: String,
        merged_task_ids: Vec<String>,
    },
    Conflict {
        merge_branch: String,
        conflict_task_id: String,
        conflict_worker_branch: String,
        merged_task_ids: Vec<String>,
        files: Vec<String>,
    },
    Failed {
        error: String,
    },
}

#[allow(dead_code)] // used via #[serde(default = "default_attempt")] — rustc doesn't track serde attrs
fn default_attempt() -> u32 {
    1
}

/// The attempt number for a task's first dispatch. Re-dispatches triggered
/// by `request_changes` bump to `entry.attempt + 1` in
/// `apply_decision_and_extract`; both initial dispatchers (`run_plan` and
/// `dispatch_newly_ready`) emit this value for their audit sentinels.
const FIRST_DISPATCH_ATTEMPT: u32 = 1;

/// Runtime state of a submitted plan.
#[derive(Debug)]
pub struct PlanState {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskEntry>,
    pub brain_session_id: BrainSessionId,
    /// Shared plan base captured at submission time. `merge_plan` builds the
    /// dedicated integration branch from this ref so merge results are
    /// reproducible and detached from later brain edits.
    pub base_snapshot_branch: Option<String>,
    /// Latest integration attempt state. Reset to `NotStarted` whenever the
    /// plan changes through review decisions.
    pub merge_state: PlanMergeState,
    /// beads epic ID when the plan was submitted with `persist_as_epic=true`.
    /// None for ephemeral plans. Currently informational only — auto-close of
    /// persist-created child issues from review_task(approve) is a planned
    /// follow-up (v1 auto-closes only `PlanTaskEntry.spec.issue_id`, the
    /// brain-supplied pre-existing ref).
    pub epic_id: Option<String>,
}

/// Maximum number of iterations per plan task. After this many attempts,
/// `review_task(request_changes)` returns an error — the brain must approve,
/// reject, or leave the task as-is.
pub const MAX_ATTEMPTS: u32 = 3;

/// Tracks the active plan for each epic so re-calling `execute_epic(epic_id)`
/// while a plan is running returns the existing plan_id. Lazy cleanup — a
/// registry entry is cleared on the next `execute_epic` call for the same
/// epic if its plan has reached a terminal overall status.
#[derive(Debug, Default)]
pub struct PlanRegistry {
    /// epic_id → plan_id (for the currently-active plan, if any).
    pub by_epic: std::collections::HashMap<String, String>,
}

/// Extract the portion of a label after a given prefix, trimmed of whitespace.
/// Returns `None` if no label starts with `prefix`.
fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels
        .iter()
        .filter_map(|l| l.strip_prefix(prefix).map(str::trim))
        .next()
}

/// Return a copy of `labels` with all entries that start with `"spur:"` removed
/// (the SPUR machine-label prefix).
// Task 2 MCP handler will consume this; tested via strip_spur_labels_drops_machine_prefix.
#[allow(dead_code)]
fn strip_spur_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|l| !l.starts_with("spur:"))
        .cloned()
        .collect()
}

/// Result of deriving a plan from a beads epic subgraph.
#[derive(Debug)]
pub struct DerivedEpicPlan {
    pub plan_tasks: Vec<PlanTask>,
    pub warnings: Vec<String>,
    pub agent_counts: std::collections::BTreeMap<String, usize>,
    pub edge_count: usize,
}

/// Pure derivation function: given a fetched epic issue, its direct children,
/// external-dependency statuses, an optional default agent, and the set of
/// configured agent names, produce a `DerivedEpicPlan` ready to hand off to
/// the existing `submit_plan` / `run_plan` engine.
///
/// Errors are returned as human-readable strings with actionable guidance.
pub fn derive_epic_plan_from_issues(
    epic: &spur_pm::Issue,
    children: &[spur_pm::Issue],
    external_dep_statuses: &std::collections::HashMap<String, String>,
    default_agent: Option<&str>,
    known_agents: &[&str],
) -> Result<DerivedEpicPlan, String> {
    // 1. Verify the root issue is actually an epic.
    if epic.issue_type.as_deref() != Some("epic") {
        let t = epic.issue_type.as_deref().unwrap_or("none");
        return Err(format!(
            "issue '{}' is not an epic (type={t}); use create_issue(type='epic') or change its type",
            epic.id
        ));
    }

    // 2. Reject empty subgraph.
    if children.is_empty() {
        return Err(format!(
            "epic '{}' has no children; create at least one child task first",
            epic.id
        ));
    }

    // 3. Build subgraph id set.
    let subgraph_ids: HashSet<&str> = children.iter().map(|c| c.id.as_str()).collect();

    let mut plan_tasks: Vec<PlanTask> = Vec::with_capacity(children.len());
    let mut warnings: Vec<String> = Vec::new();

    for child in children {
        // 4a. Reject nested epics.
        if child.issue_type.as_deref() == Some("epic") {
            return Err(format!(
                "nested epic child '{}' not supported; flatten to direct tasks",
                child.id
            ));
        }

        // 4b. Resolve agent.
        let agent = if let Some(name) = label_value(&child.labels, labels::AGENT_PREFIX) {
            name.to_string()
        } else if let Some(name) = label_value(&epic.labels, labels::AGENT_PREFIX) {
            name.to_string()
        } else if let Some(name) = default_agent {
            warnings.push(format!(
                "'{}' has no spur:agent:<name> label — used default_agent",
                child.id
            ));
            name.to_string()
        } else {
            let known = known_agents.join(", ");
            return Err(format!(
                "no agent for task '{}'; set `spur:agent:<name>` label or pass default_agent. Known agents: [{}]",
                child.id, known
            ));
        };

        // 4c. Validate agent is configured.
        if !known_agents.contains(&agent.as_str()) {
            let known = known_agents.join(", ");
            return Err(format!(
                "agent '{agent}' on task '{}' not configured. Known agents: [{}]",
                child.id, known
            ));
        }

        // 4d. Resolve task text.
        //
        // Task text comes from the issue body (description field). The former
        // `spur:task-text:<text>` label override was removed because its VALUE
        // can never round-trip through br 0.1.14's label grammar
        // `[A-Za-z0-9_:-]+` — realistic task text contains spaces, `.`, `=`,
        // etc. Task text belongs in the issue body, not a label.
        let task_text = child.body.clone();

        // 4e. Map blocked_by: keep intra-subgraph deps; validate/warn external.
        //
        // The epic's own id appears in every child's blocked_by because beads
        // flattens the `parent-child` edge into blocked_by (see
        // BLOCKING_TYPES in spur-pm/src/beads.rs). That edge is structural
        // containment, NOT an execution dependency — skip it so the pure
        // function doesn't mistakenly treat the epic as a missing external
        // dep. (The async wrapper applies the same skip when collecting
        // external_dep_statuses; both must agree.)
        let mut depends_on: Vec<String> = Vec::new();
        for b in &child.blocked_by {
            if b == &epic.id {
                continue;
            }
            if subgraph_ids.contains(b.as_str()) {
                depends_on.push(b.clone());
            } else {
                // External dep — must already be done.
                let status = external_dep_statuses
                    .get(b.as_str())
                    .map(String::as_str)
                    .unwrap_or("unknown");
                if status == "done" {
                    warnings.push(format!(
                        "external dependency '{}' is done — omitted from depends_on",
                        b
                    ));
                    // Do NOT add to depends_on; engine will treat this as Ready.
                } else {
                    return Err(format!(
                        "external dependency '{}' not done (status={status}); satisfy it or remove the edge",
                        b
                    ));
                }
            }
        }

        let id = child.id.clone();
        plan_tasks.push(PlanTask {
            task_id: id.clone(),
            agent,
            task: task_text,
            depends_on,
            issue_id: Some(id),
            context_files: vec![],
        });
    }

    // 5. Validate with existing engine (cycle detection, dangling deps, duplicates).
    validate_plan(&plan_tasks)?;

    // 6. Compute metrics.
    let mut agent_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut edge_count = 0usize;
    for t in &plan_tasks {
        *agent_counts.entry(t.agent.clone()).or_insert(0) += 1;
        edge_count += t.depends_on.len();
    }

    Ok(DerivedEpicPlan {
        plan_tasks,
        warnings,
        agent_counts,
        edge_count,
    })
}

/// Async PmService-fetching wrapper around `derive_epic_plan_from_issues`.
///
/// Fetches the epic issue, lists all issues and fetches each one, keeps only
/// direct children (issues whose `blocked_by` contains `epic_id`), fetches
/// external-dep statuses, then delegates to the pure derivation function.
///
/// `known_agents` is a slice of agent names sourced from the configured
/// `WorkerInfo` list on `McpCallbackServer`.
pub async fn derive_epic_plan(
    pm: &spur_pm::PmService,
    epic_id: &str,
    default_agent: Option<&str>,
    known_agents: &[&str],
) -> Result<DerivedEpicPlan, String> {
    // 1. Fetch the epic.
    let epic = pm
        .get_issue(epic_id)
        .await
        .map_err(|e| format!("failed to fetch epic '{epic_id}': {e}"))?;

    // 2. List all issues and fetch full details for each to find children.
    //    A child is an issue whose blocked_by list contains epic_id
    //    (the beads parent-child dependency type is included in blocked_by).
    // TODO(phase3): N+1 fetch — one get_issue per summary to detect children.
    //   Mitigate by adding IssueFilter.issue_type = Some("task") scoping, or
    //   by exposing a `parent` field on IssueSummary so children can be found
    //   without individual fetches.
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            limit: Some(500),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("failed to list issues: {e}"))?;

    let mut children: Vec<spur_pm::Issue> = Vec::new();
    for summary in &summaries {
        if summary.id == epic_id {
            continue;
        }
        let full = pm
            .get_issue(&summary.id)
            .await
            .map_err(|e| format!("failed to fetch issue '{}': {e}", summary.id))?;
        // Child detection uses blocked_by rather than a `parent` field because
        // `spur_pm::Issue` has no `parent`: the beads adapter flattens the
        // `parent-child` edge (see beads.rs BLOCKING_TYPES) into `blocked_by`.
        // Contract: `br create-issue --parent=<epic>` unconditionally inserts
        // the epic's id into the child's blocked_by. If that changes, this
        // filter silently returns zero children and execute_epic errors with
        // "epic has no children".
        if full.blocked_by.iter().any(|b| b == epic_id) {
            children.push(full);
        }
    }

    // 3. Collect external dep statuses: for each blocked_by reference in any
    //    child that is NOT in the subgraph, fetch its status.
    let subgraph_ids: std::collections::HashSet<&str> =
        children.iter().map(|c| c.id.as_str()).collect();
    let mut external_dep_statuses: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for child in &children {
        for dep in &child.blocked_by {
            if dep == epic_id || subgraph_ids.contains(dep.as_str()) {
                continue;
            }
            if external_dep_statuses.contains_key(dep) {
                continue;
            }
            let dep_issue = pm
                .get_issue(dep)
                .await
                .map_err(|e| format!("failed to fetch external dep '{dep}': {e}"))?;
            external_dep_statuses.insert(dep.clone(), dep_issue.status.clone());
        }
    }

    // 4. Delegate to the pure derivation function.
    derive_epic_plan_from_issues(
        &epic,
        &children,
        &external_dep_statuses,
        default_agent,
        known_agents,
    )
}

/// Derive a short human-readable name from a task's full text. Takes the
/// first non-empty line, trims, and caps at 60 chars on a UTF-8 boundary.
/// Used for TUI log entries and plan-status payloads so brain/user don't
/// read raw UUIDs.
pub fn display_name(spec_task: &str) -> String {
    let first = spec_task
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.len() <= 60 {
        return first.to_string();
    }
    let mut end = 60;
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    let mut s = first[..end].trim_end().to_string();
    s.push('…');
    s
}

/// Build the enriched task description used when re-dispatching a task for
/// iteration. `history` must contain every superseded attempt INCLUDING the
/// one just rejected by the current `request_changes` call — callers are
/// responsible for appending the current attempt's record before invoking
/// this function so the worker sees its most-recent predecessor's context.
///
/// `new_attempt` / `max_attempts` surface the retry budget to the worker so
/// it can self-calibrate urgency on the final attempt. No bloat cap — the
/// 3-attempt limit bounds size.
fn build_enriched_task(
    original_task: &str,
    history: &[AttemptRecord],
    current_feedback: &str,
    new_attempt: u32,
    max_attempts: u32,
) -> String {
    let mut out =
        String::with_capacity(original_task.len() + current_feedback.len() + history.len() * 512);
    out.push_str("## Original Task\n");
    out.push_str(original_task);
    let any_branch = history.iter().any(|r| r.worker_branch.is_some());
    if !history.is_empty() {
        out.push_str("\n\n## Previous Attempts\n");
        for rec in history {
            out.push_str(&format!(
                "\nAttempt {attempt} (branch {branch}):\n  Summary: {summary}\n  Diff: {diff}\n  Brain feedback: {feedback}\n",
                attempt = rec.attempt,
                branch = rec.worker_branch.as_deref().unwrap_or("—"),
                summary = rec.summary.as_deref().unwrap_or("—"),
                diff = rec
                    .diff_summary
                    .as_ref()
                    .map(|d| format!("+{}/-{} across {} files", d.insertions, d.deletions, d.files_changed))
                    .unwrap_or_else(|| "—".to_string()),
                feedback = rec.feedback,
            ));
        }
    }
    out.push_str(&format!(
        "\n## Current Request (Attempt {new_attempt} of {max_attempts})\n"
    ));
    out.push_str(current_feedback);
    out.push_str("\n\nApply the feedback above.");
    if any_branch {
        out.push_str(
            " You can inspect prior attempts with `git show <branch>` using the branch names listed above.",
        );
    }
    out.push('\n');
    out
}

// ─── Validation ──────────────────────────────────────────────────────

/// Validate a plan: check for duplicate IDs, dangling deps, and cycles.
pub fn validate_plan(tasks: &[PlanTask]) -> Result<(), String> {
    if tasks.is_empty() {
        return Err("Plan must contain at least one task".into());
    }

    // Check duplicate task_ids.
    let mut seen = HashSet::new();
    for t in tasks {
        if !seen.insert(&t.task_id) {
            return Err(format!("Duplicate task_id: '{}'", t.task_id));
        }
    }

    // Check dangling dependencies.
    let ids: HashSet<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
    for t in tasks {
        for dep in &t.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(format!(
                    "Task '{}' depends on unknown task '{dep}'",
                    t.task_id
                ));
            }
        }
    }

    // Cycle detection via Kahn's topological sort.
    if has_cycle(tasks) {
        return Err("Cycle detected in plan dependencies".into());
    }

    Ok(())
}

/// Returns true if the dependency graph contains a cycle (Kahn's algorithm).
fn has_cycle(tasks: &[PlanTask]) -> bool {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for t in tasks {
        in_degree.entry(&t.task_id).or_insert(0);
        for dep in &t.depends_on {
            adj.entry(dep.as_str()).or_default().push(&t.task_id);
            *in_degree.entry(t.task_id.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&k, _)| k)
        .collect();

    let mut sorted = 0usize;
    while let Some(node) = queue.pop_front() {
        sorted += 1;
        if let Some(neighbors) = adj.get(node) {
            for &n in neighbors {
                let deg = in_degree.get_mut(n).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(n);
                }
            }
        }
    }

    sorted != tasks.len()
}

// ─── Audit emission helpers ───────────────────────────────────────────

/// Emit a `[[spur-audit v1]] Dispatch` sentinel comment on the task issue.
/// Silently skips when `pm` is `None`, `issue_id` is `None`, or the backend
/// has no `BeadsAdvanced` surface (e.g. GitHub). Every failure is advisory —
/// logged at WARN and execution continues.
pub async fn emit_dispatch_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    plan_id: &str,
    delegation_id: &str,
    worker: &str,
    attempt: u32,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
        delegation_id: delegation_id.to_string(),
        worker: worker.to_string(),
        attempt,
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "dispatch",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Dispatch audit comment emission failed: {e}"
        );
    }
}

/// Emit a `[[spur-audit v1]] Completion` sentinel comment on the task issue.
pub async fn emit_completion_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    plan_id: &str,
    delegation_id: &str,
    worker_branch: Option<&str>,
    result_summary: Option<&str>,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::Completion {
        delegation_id: delegation_id.to_string(),
        completion_state: crate::plan::audit_sentinel::CompletionState::AwaitingReview,
        superseded: false,
        worker_branch: worker_branch.map(String::from),
        result_summary: result_summary.map(String::from),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "completion",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Completion audit comment emission failed: {e}"
        );
    }
}

/// Emit a `[[spur-audit v1]] Approval` sentinel comment on the task issue.
pub(crate) async fn emit_approval_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    plan_id: &str,
    delegation_id: &str,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::Approval {
        delegation_id: delegation_id.to_string(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "approval",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Approval audit comment emission failed: {e}"
        );
    }
}

/// Emit a `[[spur-audit v1]] Rejection` sentinel comment on the task issue.
pub(crate) async fn emit_rejection_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    plan_id: &str,
    delegation_id: &str,
    feedback: &str,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::Rejection {
        delegation_id: delegation_id.to_string(),
        feedback: feedback.to_string(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "rejection",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Rejection audit comment emission failed: {e}"
        );
    }
}

/// Build the issue update used to persist dispatch intent before work is sent.
pub fn dispatch_intent_update(delegation_id: &str) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        add_labels: vec![labels::delegation_id(delegation_id)],
        remove_labels: vec![
            format!("delegation-id:{delegation_id}"),
            "ready-for-review".to_string(),
        ],
        ..Default::default()
    }
}

/// Build the compensating update used when the dispatch send fails immediately.
pub fn dispatch_send_failure_update(delegation_id: &str) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        remove_labels: vec![labels::delegation_id(delegation_id)],
        comment: Some("Dispatch send failed before worker ownership was established.".to_string()),
        ..Default::default()
    }
}

/// Build the issue update used to clear persisted dispatch ownership.
pub fn clear_dispatch_intent_update(delegation_id: &str) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        remove_labels: vec![
            labels::delegation_id(delegation_id),
            format!("delegation-id:{delegation_id}"),
        ],
        ..Default::default()
    }
}

/// Persist dispatch intent to beads before the worker request is considered live.
pub async fn persist_dispatch_intent(
    pm: &spur_pm::PmService,
    issue_id: &str,
    plan_id: &str,
    delegation_id: &str,
    worker: &str,
    attempt: u32,
) -> anyhow::Result<()> {
    pm.update_issue(issue_id, dispatch_intent_update(delegation_id))
        .await?;
    emit_dispatch_audit(
        Some(pm as &dyn PmLike),
        &Some(issue_id.to_string()),
        plan_id,
        delegation_id,
        worker,
        attempt,
    )
    .await;
    Ok(())
}

/// Clear persisted dispatch ownership from beads.
pub async fn clear_dispatch_intent(
    pm: &spur_pm::PmService,
    issue_id: &str,
    delegation_id: &str,
) -> anyhow::Result<()> {
    pm.update_issue(issue_id, clear_dispatch_intent_update(delegation_id))
        .await
}

// ─── Executor ────────────────────────────────────────────────────────

/// Run a submitted plan to completion. Dispatches tasks through the
/// existing delegation channel in dependency order.
///
/// Spawned as a tokio task by `handle_submit_plan`. The plan state is
/// updated in-place; `get_plan_status` reads it concurrently.
pub async fn run_plan(
    plan: Arc<Mutex<PlanState>>,
    delegation_tx: mpsc::Sender<DelegationRequest>,
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pm: Option<Arc<dyn PmLike>>,
) {
    let plan_id = plan.lock().await.plan_id.clone();
    info!(plan_id = %plan_id, "Plan executor started");

    let mut in_flight = tokio::task::JoinSet::<String>::new();
    // Read once — stable for the plan's lifetime.
    let brain_sid = plan.lock().await.brain_session_id.clone();

    loop {
        // ── Single-lock pass: compute ready, mark Dispatched, collect specs ──
        let ready: Vec<(PlanTask, String)> = {
            let mut p = plan.lock().await;
            let completed: HashSet<String> = p
                .tasks
                .iter()
                .filter(|t| matches!(t.status, PlanTaskStatus::Approved { .. }))
                .map(|t| t.spec.task_id.clone())
                .collect();

            let mut batch = Vec::new();
            for entry in &mut p.tasks {
                if matches!(entry.status, PlanTaskStatus::Pending)
                    && entry
                        .spec
                        .depends_on
                        .iter()
                        .all(|d| completed.contains(d.as_str()))
                {
                    let delegation_id = uuid::Uuid::new_v4().to_string();
                    entry.status = PlanTaskStatus::Dispatched {
                        delegation_id: delegation_id.clone(),
                    };
                    entry.last_delegation_id = Some(delegation_id.clone());
                    batch.push((entry.spec.clone(), delegation_id));
                }
            }
            batch
        }; // Lock released.

        for (task_spec, delegation_id) in ready {
            let (tx, rx) = oneshot::channel::<DelegationResult>();

            let request = DelegationRequest {
                id: delegation_id.clone().into(),
                agent: task_spec.agent.clone(),
                task: task_spec.task.clone(),
                context_files: task_spec.context_files.clone(),
                respond_to: tx,
                brain_session_id: brain_sid.clone(),
                delegation_plan: None,
                issue_id: task_spec.issue_id.clone(),
            };

            if let Err(e) = delegation_tx.send(request).await {
                warn!(
                    plan_id = %plan_id,
                    task_id = %task_spec.task_id,
                    "Failed to dispatch plan task: {e}"
                );
                let mut p = plan.lock().await;
                if let Some(entry) = p
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_spec.task_id)
                {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Delegation channel closed".into(),
                    };
                }
                continue;
            }

            debug!(
                plan_id = %plan_id,
                task_id = %task_spec.task_id,
                agent = %task_spec.agent,
                "Plan task dispatched"
            );

            // Emit Dispatch audit sentinel — outside the plan lock.
            emit_dispatch_audit(
                pm.as_deref(),
                &task_spec.issue_id,
                &plan_id,
                &delegation_id,
                &task_spec.agent,
                FIRST_DISPATCH_ATTEMPT,
            )
            .await;

            let tid = task_spec.task_id.clone();
            let plan_ref = Arc::clone(&plan);
            let pid = plan_id.clone();
            let pm_ref = pm.clone();
            let issue_id_for_completion = task_spec.issue_id.clone();
            let delegation_id_for_completion = delegation_id.clone();

            in_flight.spawn(async move {
                match rx.await {
                    Ok(result) => {
                        // Collect completion data before acquiring lock.
                        let completion_success = matches!(
                            result.status,
                            DelegationStatus::Success | DelegationStatus::Modified { .. }
                        );
                        let result_worker_branch = result.worker_branch.clone();
                        let result_summary = result.summary.clone();

                        let mut p = plan_ref.lock().await;
                        if let Some(entry) =
                            p.tasks.iter_mut().find(|t| t.spec.task_id == tid)
                        {
                            match &result.status {
                                DelegationStatus::Success | DelegationStatus::Modified { .. } => {
                                    info!(plan_id = %pid, task_id = %tid, "Plan task awaiting review");
                                    entry.status = PlanTaskStatus::AwaitingReview {
                                        summary: result.summary.clone(),
                                    };
                                    entry.worker_branch = result.worker_branch.clone();
                                }
                                DelegationStatus::Failed { error } => {
                                    warn!(plan_id = %pid, task_id = %tid, "Plan task failed: {error}");
                                    entry.status = PlanTaskStatus::Failed {
                                        error: error.clone(),
                                    };
                                }
                                DelegationStatus::Cancelled { reason } => {
                                    warn!(plan_id = %pid, task_id = %tid, "Plan task cancelled: {reason}");
                                    entry.status = PlanTaskStatus::Cancelled {
                                        reason: reason.clone(),
                                    };
                                }
                                other => {
                                    warn!(plan_id = %pid, task_id = %tid, "Plan task ended: {other:?}");
                                    entry.status = PlanTaskStatus::Failed {
                                        error: format!("{other:?}"),
                                    };
                                }
                            }
                            entry.result = Some(result);
                        }
                        // Lock released at end of block — emit Completion audit outside.
                        drop(p);
                        if completion_success {
                            emit_completion_audit(
                                pm_ref.as_deref(),
                                &issue_id_for_completion,
                                &pid,
                                &delegation_id_for_completion,
                                result_worker_branch.as_deref(),
                                result_summary.as_deref(),
                            )
                            .await;
                        }
                    }
                    Err(_) => {
                        let mut p = plan_ref.lock().await;
                        if let Some(entry) =
                            p.tasks.iter_mut().find(|t| t.spec.task_id == tid)
                        {
                            entry.status = PlanTaskStatus::Failed {
                                error: "Orchestrator channel dropped".into(),
                            };
                        }
                    }
                }
                tid
            });
        }

        // ── Wait for next completion ─────────────────────────────────
        if in_flight.is_empty() {
            // B0: exit only when no further progression is possible. The
            // three states that can still drive new transitions are:
            //   - Dispatched: a worker may produce a result (via the
            //     handle_review_task → dispatch_newly_ready post-review
            //     path, whose completion futures live on a separate
            //     TaskTracker and are not tracked in run_plan's in_flight).
            //   - Ready: scheduled but not yet sent — next iteration
            //     will dispatch.
            //   - AwaitingReview: the brain may yet approve / reject /
            //     request_changes. Exiting here would race with
            //     handle_review_task and cause DN-6 cleanup to stamp the
            //     task as Failed with "Plan exited with task awaiting
            //     review" — exactly the phase-3b dispatch failure.
            // Pending with only-terminal deps is truly stuck and should
            // fall through to DN-6 cleanup. Reviews are human-paced, so
            // the poll interval is coarse.
            let any_progressable = {
                let p = plan.lock().await;
                p.tasks.iter().any(|t| {
                    matches!(
                        t.status,
                        PlanTaskStatus::Dispatched { .. }
                            | PlanTaskStatus::Ready
                            | PlanTaskStatus::AwaitingReview { .. }
                    )
                })
            };
            if !any_progressable {
                break; // DN-6 cleanup will stamp any Pending orphans.
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            continue;
        }

        // Await the next completed task.
        match in_flight.join_next().await {
            Some(Ok(_task_id)) => {
                // Status already updated inside the spawned future.
                // Loop back to check for newly-ready tasks.
                continue;
            }
            Some(Err(e)) => {
                warn!(plan_id = %plan_id, "Plan task join error: {e}");
                continue;
            }
            None => break,
        }
    }

    // DN-6: On terminal loop exit, promote all non-terminal tasks to Failed with
    // a specific error. Originally this block only caught Pending-with-failed-dep;
    // now it also catches Pending with missing deps, stuck Ready, stuck Dispatched,
    // and stuck AwaitingReview. Approved / Rejected / Failed pass through unchanged.
    {
        let mut p = plan.lock().await;
        let failed_ids: HashSet<String> = p
            .tasks
            .iter()
            .filter(|t| matches!(t.status, PlanTaskStatus::Failed { .. }))
            .map(|t| t.spec.task_id.clone())
            .collect();

        for entry in &mut p.tasks {
            match &entry.status {
                PlanTaskStatus::Pending
                    if entry.spec.depends_on.iter().any(|d| failed_ids.contains(d)) =>
                {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Blocked by failed dependency".into(),
                    };
                }
                PlanTaskStatus::Pending => {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Plan exited with task still pending (dep never satisfied)".into(),
                    };
                }
                PlanTaskStatus::Ready => {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Plan exited with task ready but never dispatched".into(),
                    };
                }
                PlanTaskStatus::Dispatched { delegation_id } => {
                    let err =
                        format!("Plan exited with task still running (delegation {delegation_id})");
                    entry.status = PlanTaskStatus::Failed { error: err };
                }
                PlanTaskStatus::AwaitingReview { .. } => {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Plan exited with task awaiting review".into(),
                    };
                }
                PlanTaskStatus::Approved { .. }
                | PlanTaskStatus::Rejected { .. }
                | PlanTaskStatus::Failed { .. }
                | PlanTaskStatus::Cancelled { .. }
                | PlanTaskStatus::Superseded { .. } => {}
            }
        }
    }

    // INV-7: Compute terminal counts and emit lifecycle events OUTSIDE the lock.
    let (
        approved_count,
        rejected_count,
        failed_count,
        cancelled_count,
        awaiting_review_count,
        all_approved,
    ) = {
        let p = plan.lock().await;
        let mut a = 0u32;
        let mut r = 0u32;
        let mut f = 0u32;
        let mut c = 0u32;
        let mut ar = 0u32;
        let non_empty = !p.tasks.is_empty();
        let mut all_a = non_empty;
        for t in &p.tasks {
            match &t.status {
                PlanTaskStatus::Approved { .. } => a += 1,
                PlanTaskStatus::Rejected { .. } => {
                    r += 1;
                    all_a = false;
                }
                PlanTaskStatus::Failed { .. } => {
                    f += 1;
                    all_a = false;
                }
                PlanTaskStatus::Cancelled { .. } => {
                    c += 1;
                    all_a = false;
                }
                PlanTaskStatus::AwaitingReview { .. } => {
                    ar += 1;
                    all_a = false;
                }
                // Pending / Ready / Dispatched / Iterating: not terminal, not all_approved.
                _ => {
                    all_a = false;
                }
            }
        }
        (a, r, f, c, ar, all_a)
    }; // Lock released before emitting.

    info!(
        plan_id = %plan_id,
        awaiting_review = awaiting_review_count,
        approved = approved_count,
        failed = failed_count,
        cancelled = cancelled_count,
        "Plan executor finished"
    );

    if let Some(sink) = &event_sink {
        sink.emit(spur_acp::SpurEventBody::PlanCompleted {
            plan_id: plan_id.clone(),
            approved: approved_count,
            rejected: rejected_count,
            failed: failed_count,
            cancelled: cancelled_count,
        });
        if all_approved && cancelled_count == 0 {
            sink.emit(spur_acp::SpurEventBody::PlanReadyToMerge {
                plan_id: plan_id.clone(),
            });
        }
    }
}

// ─── Status rendering ────────────────────────────────────────────────

/// Build a JSON-serializable status report for a plan.
pub fn build_plan_status(plan_id: &str, state: &PlanState) -> serde_json::Value {
    let total = state.tasks.len();

    // Count tasks per state.
    let mut n_pending = 0usize;
    let mut n_ready = 0usize;
    let mut n_dispatched = 0usize;
    let mut n_awaiting_review = 0usize;
    let mut n_approved = 0usize;
    let mut n_rejected = 0usize;
    let mut n_failed = 0usize;
    let mut n_cancelled = 0usize;

    for t in &state.tasks {
        match &t.status {
            PlanTaskStatus::Pending => n_pending += 1,
            PlanTaskStatus::Ready => n_ready += 1,
            PlanTaskStatus::Dispatched { .. } => n_dispatched += 1,
            PlanTaskStatus::AwaitingReview { .. } => n_awaiting_review += 1,
            PlanTaskStatus::Approved { .. } => n_approved += 1,
            PlanTaskStatus::Rejected { .. } => n_rejected += 1,
            PlanTaskStatus::Failed { .. } => n_failed += 1,
            PlanTaskStatus::Cancelled { .. } => n_cancelled += 1,
            // v0b: Superseded is a terminal "no outcome" state — fold with
            // cancelled for aggregate metrics. Lineage tracked via `by`.
            PlanTaskStatus::Superseded { .. } => n_cancelled += 1,
        }
    }

    let all_workers_done = n_dispatched == 0 && n_pending == 0 && n_ready == 0;
    let ready_to_merge = all_workers_done
        && n_awaiting_review == 0
        && n_rejected == 0
        && n_failed == 0
        && n_cancelled == 0
        && n_approved == total;

    let overall = if n_dispatched > 0 || n_pending > 0 || n_ready > 0 {
        "running"
    } else if n_awaiting_review > 0 {
        "awaiting_review"
    } else if n_approved == total && total > 0 {
        "approved"
    } else if n_failed == total && total > 0 {
        "failed"
    } else if n_failed > 0 {
        "has_failures"
    } else if n_rejected > 0 {
        "has_rejections"
    } else {
        "partial"
    };

    let reviewed = n_approved + n_rejected;

    let tasks_json: Vec<serde_json::Value> = state
        .tasks
        .iter()
        .map(|t| {
            let mut obj = serde_json::json!({
                "task_id": t.spec.task_id,
                "task_name": display_name(&t.spec.task),
                "agent": t.spec.agent,
                "attempt": t.attempt,
                "max_attempts": MAX_ATTEMPTS,
                "history_count": t.history.len(),
            });
            match &t.status {
                PlanTaskStatus::Pending => {
                    obj["status"] = "pending".into();
                    let blocked_by: Vec<&str> = t
                        .spec
                        .depends_on
                        .iter()
                        .filter(|d| {
                            !state.tasks.iter().any(|o| {
                                o.spec.task_id == **d
                                    && matches!(
                                        o.status,
                                        PlanTaskStatus::Approved { .. }
                                            | PlanTaskStatus::Cancelled { .. }
                                    )
                            })
                        })
                        .map(|d| d.as_str())
                        .collect();
                    if !blocked_by.is_empty() {
                        obj["blocked_by"] = serde_json::json!(blocked_by);
                    }
                }
                PlanTaskStatus::Ready => {
                    obj["status"] = "ready".into();
                }
                PlanTaskStatus::Dispatched { delegation_id } => {
                    obj["status"] = "dispatched".into();
                    obj["delegation_id"] = delegation_id.clone().into();
                }
                PlanTaskStatus::AwaitingReview { summary }
                | PlanTaskStatus::Approved { summary } => {
                    let status_str = if matches!(t.status, PlanTaskStatus::AwaitingReview { .. }) {
                        "awaiting_review"
                    } else {
                        "approved"
                    };
                    obj["status"] = status_str.into();
                    if matches!(t.status, PlanTaskStatus::AwaitingReview { .. }) {
                        obj["remaining_attempts"] = MAX_ATTEMPTS.saturating_sub(t.attempt).into();
                    }
                    if let Some(s) = summary {
                        obj["summary"] = s.clone().into();
                    }
                    if let Some(ref wb) = t.worker_branch {
                        obj["worker_branch"] = wb.clone().into();
                    }
                    if let Some(ref result) = t.result {
                        if let Some(ref ds) = result.diff_summary {
                            if let Ok(v) = serde_json::to_value(ds) {
                                obj["diff_summary"] = v;
                            }
                        }
                        if let Some(ref art) = result.artifact {
                            obj["artifact"] = serde_json::json!({
                                "object_ref": art.object_ref,
                                "blob_sha": art.blob_sha,
                                "size_bytes": art.size_bytes,
                                "kind": art.kind,
                                "retrieval_hint": format!(
                                    "git cat-file -p {}",
                                    art.object_ref
                                ),
                            });
                        }
                    }
                }
                PlanTaskStatus::Rejected { feedback } => {
                    obj["status"] = "rejected".into();
                    if let Some(f) = feedback {
                        obj["feedback"] = f.clone().into();
                    }
                    if let Some(ref wb) = t.worker_branch {
                        obj["worker_branch"] = wb.clone().into();
                    }
                    if let Some(ref result) = t.result {
                        if let Some(ref art) = result.artifact {
                            obj["artifact"] = serde_json::json!({
                                "object_ref": art.object_ref,
                                "blob_sha": art.blob_sha,
                                "size_bytes": art.size_bytes,
                                "kind": art.kind,
                                "retrieval_hint": format!(
                                    "git cat-file -p {}",
                                    art.object_ref
                                ),
                            });
                        }
                    }
                }
                PlanTaskStatus::Failed { error } => {
                    obj["status"] = "failed".into();
                    obj["error"] = error.clone().into();
                }
                PlanTaskStatus::Cancelled { reason } => {
                    obj["status"] = "cancelled".into();
                    obj["reason"] = reason.clone().into();
                }
                PlanTaskStatus::Superseded { mutation_id, by } => {
                    obj["status"] = "superseded".into();
                    obj["mutation_id"] = mutation_id.clone().into();
                    obj["superseded_by"] = serde_json::json!(by);
                }
            }
            obj
        })
        .collect();

    let merge_json = match &state.merge_state {
        PlanMergeState::NotStarted => serde_json::json!({
            "status": "not_started",
            "base_snapshot_branch": state.base_snapshot_branch,
        }),
        PlanMergeState::Succeeded {
            merge_branch,
            merged_task_ids,
        } => serde_json::json!({
            "status": "succeeded",
            "base_snapshot_branch": state.base_snapshot_branch,
            "merge_branch": merge_branch,
            "merged_task_ids": merged_task_ids,
        }),
        PlanMergeState::Conflict {
            merge_branch,
            conflict_task_id,
            conflict_worker_branch,
            merged_task_ids,
            files,
        } => serde_json::json!({
            "status": "conflict",
            "base_snapshot_branch": state.base_snapshot_branch,
            "merge_branch": merge_branch,
            "conflict_task_id": conflict_task_id,
            "conflict_worker_branch": conflict_worker_branch,
            "merged_task_ids": merged_task_ids,
            "files": files,
        }),
        PlanMergeState::Failed { error } => serde_json::json!({
            "status": "failed",
            "base_snapshot_branch": state.base_snapshot_branch,
            "error": error,
        }),
    };

    let next_action = match overall {
        "running" => "Workers still running. Poll get_plan_status to monitor.".to_string(),
        "awaiting_review" => {
            "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                .to_string()
        }
        "approved" => match &state.merge_state {
            PlanMergeState::NotStarted => {
                "All tasks approved. Use merge_plan to create a dedicated integration branch."
                    .to_string()
            }
            PlanMergeState::Succeeded { merge_branch, .. } => format!(
                "Plan merged to '{}'. Use create_pr with that branch to create a pull request.",
                merge_branch
            ),
            PlanMergeState::Conflict {
                merge_branch,
                conflict_task_id,
                ..
            } => format!(
                "merge_plan hit a conflict on task '{}' while building '{}'. Resolve the integration branch manually or revise and rerun.",
                conflict_task_id, merge_branch
            ),
            PlanMergeState::Failed { error } => format!("merge_plan failed: {error}"),
        },
        "has_failures" => "Some tasks failed. Use get_task_diff to inspect failures.".to_string(),
        "has_rejections" => "Some tasks rejected. Revise the plan or re-submit.".to_string(),
        "failed" => "All tasks failed. Use get_task_diff to inspect errors.".to_string(),
        _ => String::new(),
    };

    serde_json::json!({
        "plan_id": plan_id,
        "status": overall,
        "progress": format!(
            "{reviewed}/{total} reviewed, {n_dispatched} running, {n_pending} pending, {n_failed} failed"
        ),
        "counts": {
            "total": total,
            "pending": n_pending,
            "ready": n_ready,
            "dispatched": n_dispatched,
            "awaiting_review": n_awaiting_review,
            "approved": n_approved,
            "rejected": n_rejected,
            "failed": n_failed,
        },
        "all_workers_done": all_workers_done,
        "ready_to_merge": ready_to_merge,
        "next_action": next_action,
        "merge": merge_json,
        "tasks": tasks_json,
    })
}

/// True iff the given overall plan status (as returned by
/// `build_plan_status`'s `"status"` field) is a terminal state — no further
/// task transitions will happen without brain intervention. Non-terminal
/// plans can still receive worker results or brain reviews.
pub fn is_terminal_plan_status(overall: &str) -> bool {
    matches!(
        overall,
        "approved" | "failed" | "has_failures" | "has_rejections" | "partial"
    )
}

/// Format the beads comment body for a `request_changes` review decision.
/// Pure — no I/O. Written to the PM backend after the new worker attempt
/// has been successfully dispatched.
pub(crate) fn format_request_changes_comment(
    feedback: &str,
    attempt: u32,
    max_attempts: u32,
    worker_branch: Option<&str>,
) -> String {
    let branch_line = worker_branch.unwrap_or("(no branch yet)");
    format!(
        "Brain requested changes (attempt {attempt}/{max_attempts}):\n{feedback}\n\nWorker branch: {branch_line}"
    )
}

/// Build the JSON fields for a `get_task_diff` response given a
/// DelegationResult. Pure — no I/O. Owns the contract that the "diff"
/// key is ALWAYS present when the task has a result; when the diff is
/// None, a structured marker tells the brain why (Option E from
/// docs/rca/2026-04-18-get-task-diff-empty.md).
pub(crate) fn build_task_diff_fields(
    result: &spur_acp::DelegationResult,
) -> Vec<(String, serde_json::Value)> {
    use serde_json::json;
    let mut out: Vec<(String, serde_json::Value)> = Vec::new();

    match &result.diff {
        Some(diff) => {
            out.push(("diff".into(), json!(diff)));
        }
        None => {
            out.push(("diff".into(), serde_json::Value::Null));
            out.push(("diff_status".into(), json!("no_changes_detected")));
            out.push(("diff_basis".into(), json!("base_commit..HEAD")));
        }
    }
    if let Some(ref ds) = result.diff_summary {
        out.push((
            "diff_summary".into(),
            serde_json::to_value(ds).unwrap_or_default(),
        ));
    }
    if let Some(ref s) = result.summary {
        out.push(("summary".into(), json!(s)));
    }
    if let Some(art) = &result.artifact {
        out.push((
            "artifact".into(),
            json!({
                "object_ref": art.object_ref,
                "blob_sha": art.blob_sha,
                "size_bytes": art.size_bytes,
                "kind": art.kind,
                "retrieval_hint": format!(
                    "git cat-file -p {}",
                    art.object_ref
                ),
            }),
        ));
    }
    out
}

/// Review a task in a plan: approve, reject, or request_changes.
/// Optionally syncs with beads (pm), emits events (sink), and dispatches
/// newly-ready tasks on approval (delegation_tx / task_tracker / plan_arc).
///
/// # Test-only
/// This function is compiled only in `#[cfg(test)]` builds. Production code
/// must use `handle_review_task`, which drops the plan lock before beads I/O.
/// Having a separate non-production path avoids divergence risk; the test
/// exercises the beads-warning path (pm passed inline) which `handle_review_task`
/// intentionally omits from its synchronous phase.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
    sink: Option<&dyn crate::events::McpEventSink>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
    plan_arc: Option<std::sync::Arc<tokio::sync::Mutex<PlanState>>>,
) -> Result<serde_json::Value, String> {
    let mut warnings: Vec<String> = Vec::new();

    // Validate the task exists and is in AwaitingReview.
    let (summary, current_attempt) = {
        let entry = state
            .tasks
            .iter()
            .find(|t| t.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;
        match &entry.status {
            PlanTaskStatus::AwaitingReview { summary } => (summary.clone(), entry.attempt),
            other => {
                let name = match other {
                    PlanTaskStatus::Pending => "pending",
                    PlanTaskStatus::Ready => "ready",
                    PlanTaskStatus::Dispatched { .. } => "dispatched",
                    PlanTaskStatus::Approved { .. } => "approved",
                    PlanTaskStatus::Rejected { .. } => "rejected",
                    PlanTaskStatus::Failed { .. } => "failed",
                    _ => "unknown",
                };
                return Err(format!(
                    "task '{task_id}' is not awaiting review (current status: {name})"
                ));
            }
        }
    };

    // new_dispatches: (task_id, attempt, delegation_id) for each task newly dispatched.
    let mut new_dispatches: Vec<(String, u32, String)> = Vec::new();

    match decision {
        "approve" => {
            // Mark Approved.
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            entry.status = PlanTaskStatus::Approved {
                summary: summary.clone(),
            };
            let issue_id = entry.spec.issue_id.clone();

            // Beads sync (non-blocking).
            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }

            // Approval cascade: dispatch any Pending tasks whose deps are now all Approved.
            if let (Some(tx), Some(tracker), Some(arc)) =
                (delegation_tx, task_tracker, plan_arc.clone())
            {
                let mut _unused_audit_emits: Vec<PendingAuditEmit> = Vec::new();
                // test-only review_task has no Arc<dyn PmLike> — audit no-ops.
                dispatch_newly_ready(
                    plan_id,
                    state,
                    tx,
                    tracker,
                    arc,
                    sink,
                    &mut warnings,
                    &mut new_dispatches,
                    &mut _unused_audit_emits,
                    None,
                );
            }
        }
        "reject" => {
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            entry.status = PlanTaskStatus::Rejected {
                feedback: feedback.map(String::from),
            };
            let issue_id = entry.spec.issue_id.clone();

            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain rejected: {}",
                        feedback.unwrap_or("does not meet requirements")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some("open".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }

            // Rejection cascade: mark all transitively-dependent tasks as Failed.
            mark_descendants_failed(task_id, state, &mut warnings);
        }
        "request_changes" => {
            let fb = feedback.ok_or_else(|| "request_changes requires feedback".to_string())?;

            // At MAX_ATTEMPTS, auto-transition to Rejected instead of erroring
            // and leaving the task in AwaitingReview limbo. Reuses the existing
            // rejection cascade and PlanTaskReviewed event. Downstream consumers
            // (is_terminal_plan_status, TUI, retry_plan_task sentinel) all
            // already handle Rejected correctly.
            {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                if entry.attempt >= MAX_ATTEMPTS {
                    let exhausted_fb = format!(
                        "retries exhausted ({}/{}): {}",
                        entry.attempt, MAX_ATTEMPTS, fb
                    );
                    let issue_id = entry.spec.issue_id.clone();
                    let attempt_at_reject = entry.attempt;
                    entry.status = PlanTaskStatus::Rejected {
                        feedback: Some(exhausted_fb.clone()),
                    };
                    warnings.push(format!(
                        "auto-rejected: MAX_ATTEMPTS ({MAX_ATTEMPTS}) reached"
                    ));

                    // Rejection cascade.
                    mark_descendants_failed(task_id, state, &mut warnings);

                    // Best-effort beads comment.
                    if let Some(pm) = pm {
                        if let Some(ref id) = issue_id {
                            let comment = format!(
                                "Brain rejected (retries exhausted {}/{}): {}",
                                attempt_at_reject, MAX_ATTEMPTS, fb
                            );
                            let update = spur_pm::IssueUpdate {
                                status: Some("open".to_string()),
                                comment: Some(comment),
                                ..Default::default()
                            };
                            if let Err(e) = pm.update_issue(id, update).await {
                                warnings.push(format!("beads comment failed: {e}"));
                            }
                        }
                    }

                    // Build response and early-return with decision=reject.
                    let task_name = state
                        .tasks
                        .iter()
                        .find(|t| t.spec.task_id == task_id)
                        .map(|t| display_name(&t.spec.task))
                        .unwrap_or_default();
                    let mut resp = build_plan_status(plan_id, state);
                    if let serde_json::Value::Object(ref mut m) = resp {
                        m.insert("task_id".into(), serde_json::json!(task_id));
                        m.insert("task_name".into(), serde_json::json!(task_name));
                        m.insert("decision".into(), serde_json::json!("reject"));
                        m.insert("warnings".into(), serde_json::json!(warnings));
                    }
                    if let Some(sink) = sink {
                        sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
                            plan_id: plan_id.to_string(),
                            task_id: task_id.to_string(),
                            task_name: Some(task_name),
                            decision: "reject".to_string(),
                            feedback: Some(exhausted_fb),
                            attempt: attempt_at_reject,
                            max_attempts: MAX_ATTEMPTS,
                        });
                    }
                    return Ok(resp);
                }
            }

            // --- normal request_changes path (unchanged) ---

            let (tx, tracker, arc) = match (delegation_tx, task_tracker, plan_arc.clone()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => {
                    return Err(
                        "request_changes requires orchestrator channel (internal error)"
                            .to_string(),
                    );
                }
            };

            // Re-bind entry mutably for the normal path (the MAX_ATTEMPTS
            // check above used a scoped borrow that has been dropped).
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();

            // Capture the attempt being superseded so it appears in the
            // enriched task (the worker must see its most-recent predecessor's
            // branch/summary/diff, not just older history). Cloned for the
            // pre-send snapshot; the real record is pushed on commit below.
            let current_record = AttemptRecord {
                attempt: entry.attempt,
                worker_branch: entry.worker_branch.clone(),
                diff_summary: entry.result.as_ref().and_then(|r| r.diff_summary.clone()),
                summary: entry.result.as_ref().and_then(|r| r.summary.clone()),
                feedback: fb.to_string(),
            };

            let new_attempt = entry.attempt + 1;

            // Build enriched task with full history INCLUDING the attempt
            // just superseded. Read-only snapshot — if try_send fails below,
            // entry state is unchanged and the brain can retry review_task.
            let mut history_snapshot = entry.history.clone();
            history_snapshot.push(current_record.clone());
            let enriched = build_enriched_task(
                &entry.spec.task,
                &history_snapshot,
                fb,
                new_attempt,
                MAX_ATTEMPTS,
            );

            let delegation_id = uuid::Uuid::new_v4().to_string();
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<DelegationResult>();

            let req = crate::tools::DelegationRequest {
                id: delegation_id.clone().into(),
                agent: entry.spec.agent.clone(),
                task: enriched,
                delegation_plan: None,
                issue_id: entry.spec.issue_id.clone(),
                context_files: entry.spec.context_files.clone(),
                respond_to: resp_tx,
                brain_session_id: state.brain_session_id.clone(),
            };

            // try_send — atomic. Fail fast if channel full/closed.
            if let Err(e) = tx.try_send(req) {
                return Err(format!("orchestrator channel error: {e}"));
            }

            // Mutate state AFTER successful send. Clear the superseded
            // attempt's latest-slot fields (worker_branch was only cloned
            // above; take it now so the invariant "worker_branch is latest
            // only" holds after the push).
            entry.worker_branch = None;
            entry.history.push(current_record);
            entry.result = None;
            entry.attempt = new_attempt;
            entry.status = PlanTaskStatus::Dispatched {
                delegation_id: delegation_id.clone(),
            };

            // Spawn completion future. test-only review_task does not emit the
            // Completion audit sentinel — pm threading is covered by
            // apply_decision_and_extract's request_changes path.
            spawn_completion_future(
                task_id.to_string(),
                delegation_id.clone(),
                resp_rx,
                arc,
                tracker,
                None,
                plan_id.to_string(),
            );

            new_dispatches.push((task_id.to_string(), new_attempt, delegation_id));

            // Best-effort audit write to beads. Runs AFTER try_send has
            // succeeded and state mutation is committed — if this fails the
            // dispatch still happened, which is what the state machine
            // already reflects. Pulls the worker_branch from
            // entry.history.last() (the just-superseded attempt) rather
            // than entry.worker_branch (cleared at line ~1125 above).
            //
            // NOTE: entry is borrowed mutably at this point from earlier in
            // the request_changes arm. We release the borrow before the
            // await by cloning out the fields we need.
            let issue_id_for_audit = entry.spec.issue_id.clone();
            let superseded_branch: Option<String> =
                entry.history.last().and_then(|h| h.worker_branch.clone());
            if let (Some(pm), Some(id)) = (pm, issue_id_for_audit.as_ref()) {
                // Comment reports the attempt just reviewed (new_attempt - 1), not the one being dispatched.
                let comment = format_request_changes_comment(
                    fb,
                    new_attempt - 1,
                    MAX_ATTEMPTS,
                    superseded_branch.as_deref(),
                );
                let update = spur_pm::IssueUpdate {
                    comment: Some(comment),
                    ..Default::default()
                };
                if let Err(e) = pm.update_issue(id, update).await {
                    warnings.push(format!("beads comment failed: {e}"));
                }
            }
        }
        other => {
            return Err(format!(
                "invalid decision '{other}': must be 'approve', 'reject', or 'request_changes'"
            ));
        }
    }

    // Look up display name for this task (used in both response + events).
    let task_name = state
        .tasks
        .iter()
        .find(|t| t.spec.task_id == task_id)
        .map(|t| display_name(&t.spec.task))
        .unwrap_or_default();

    // Build response (uses updated state).
    let mut resp = build_plan_status(plan_id, state);
    if let serde_json::Value::Object(ref mut m) = resp {
        m.insert("task_id".into(), serde_json::json!(task_id));
        m.insert("task_name".into(), serde_json::json!(task_name));
        m.insert("decision".into(), serde_json::json!(decision));
        m.insert("warnings".into(), serde_json::json!(warnings));
        if decision == "request_changes" {
            if let Some((_, new_att, did)) =
                new_dispatches.iter().find(|(tid, _, _)| tid == task_id)
            {
                m.insert("new_attempt".into(), serde_json::json!(new_att));
                m.insert("new_delegation_id".into(), serde_json::json!(did));
                m.insert("max_attempts".into(), serde_json::json!(MAX_ATTEMPTS));
                m.insert(
                    "remaining_attempts".into(),
                    serde_json::json!(MAX_ATTEMPTS.saturating_sub(*new_att)),
                );
            }
        }
    }

    // Emit events.
    if let Some(sink) = sink {
        sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            task_name: Some(task_name.clone()),
            decision: decision.to_string(),
            feedback: feedback.map(String::from),
            attempt: current_attempt,
            max_attempts: MAX_ATTEMPTS,
        });
        for (tid, attempt, did) in &new_dispatches {
            if tid == task_id && decision == "request_changes" {
                sink.emit(spur_acp::SpurEventBody::PlanTaskIterating {
                    plan_id: plan_id.to_string(),
                    task_id: tid.clone(),
                    task_name: Some(task_name.clone()),
                    attempt: *attempt,
                    max_attempts: MAX_ATTEMPTS,
                    delegation_id: did.clone(),
                });
            }
        }
    }

    Ok(resp)
}

// ─── INV-5: plan-lock-free review ────────────────────────────────────────────

/// Trait abstraction over `PmService` so tests can inject a sleeping mock
/// without standing up a real beads/GitHub backend.
#[async_trait::async_trait]
pub trait PmLike: Send + Sync + 'static {
    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()>;
    fn closed_status(&self) -> &str;
    /// Returns the `BeadsAdvanced` extension surface if the backend is beads.
    /// Returns `None` for non-beads backends (GitHub) and test fakes.
    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        None
    }
}

#[async_trait::async_trait]
impl PmLike for spur_pm::PmService {
    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        spur_pm::PmService::update_issue(self, id, update).await
    }
    fn closed_status(&self) -> &str {
        spur_pm::PmService::closed_status(self)
    }
    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        spur_pm::PmService::advanced(self)
    }
}

/// Pending beads I/O: collected by `apply_decision_and_extract`, executed
/// outside the plan lock by `handle_review_task`.
struct PendingBeadsOp {
    issue_id: String,
    update: spur_pm::IssueUpdate,
}

/// Events to be emitted after the plan lock is released.
enum PendingEvent {
    TaskReviewed {
        plan_id: String,
        task_id: String,
        task_name: Option<String>,
        decision: String,
        feedback: Option<String>,
        attempt: u32,
    },
    TaskIterating {
        plan_id: String,
        task_id: String,
        task_name: Option<String>,
        attempt: u32,
        delegation_id: String,
    },
}

/// Pending audit sentinel emission: collected by `apply_decision_and_extract`
/// (sync, under the plan lock) and flushed by `handle_review_task` after the
/// lock is released. Advisory — failures are logged at WARN and continue.
enum PendingAuditEmit {
    Approval {
        issue_id: Option<String>,
        plan_id: String,
        delegation_id: String,
    },
    Rejection {
        issue_id: Option<String>,
        plan_id: String,
        delegation_id: String,
        feedback: String,
    },
    Dispatch {
        issue_id: Option<String>,
        plan_id: String,
        delegation_id: String,
        worker: String,
        attempt: u32,
    },
}

/// Everything produced by `apply_decision_and_extract` under the plan lock.
struct DecisionOutcome {
    /// JSON response to return to the caller.
    resp: serde_json::Value,
    /// Beads updates to execute after the lock is released.
    beads_ops: Vec<PendingBeadsOp>,
    /// Events to emit after the lock is released.
    events: Vec<PendingEvent>,
    /// Audit sentinel emissions to flush after the lock is released.
    audit_emits: Vec<PendingAuditEmit>,
}

/// Sync state-mutation half of `handle_review_task`.
/// All `.await` points live in the caller; this function MUST remain sync.
///
/// Returns `Err(String)` for validation failures (unknown task, wrong status,
/// missing feedback). Returns `Ok(DecisionOutcome)` on success.
#[allow(clippy::too_many_arguments)]
fn apply_decision_and_extract(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm_closed_status: Option<&str>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
    plan_arc: Option<std::sync::Arc<tokio::sync::Mutex<PlanState>>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    pm_arc: Option<&Arc<dyn PmLike>>,
) -> Result<DecisionOutcome, String> {
    let mut warnings: Vec<String> = Vec::new();
    let mut beads_ops: Vec<PendingBeadsOp> = Vec::new();
    let mut audit_emits: Vec<PendingAuditEmit> = Vec::new();

    // Validate the task exists and is in AwaitingReview.
    let (summary, current_attempt) = {
        let entry = state
            .tasks
            .iter()
            .find(|t| t.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;
        match &entry.status {
            PlanTaskStatus::AwaitingReview { summary } => (summary.clone(), entry.attempt),
            other => {
                let name = match other {
                    PlanTaskStatus::Pending => "pending",
                    PlanTaskStatus::Ready => "ready",
                    PlanTaskStatus::Dispatched { .. } => "dispatched",
                    PlanTaskStatus::Approved { .. } => "approved",
                    PlanTaskStatus::Rejected { .. } => "rejected",
                    PlanTaskStatus::Failed { .. } => "failed",
                    _ => "unknown",
                };
                return Err(format!(
                    "task '{task_id}' is not awaiting review (current status: {name})"
                ));
            }
        }
    };

    let mut new_dispatches: Vec<(String, u32, String)> = Vec::new();

    match decision {
        "approve" => {
            state.merge_state = PlanMergeState::NotStarted;
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            let issue_id = entry.spec.issue_id.clone();
            let last_del_id = entry.last_delegation_id.clone();
            entry.status = PlanTaskStatus::Approved {
                summary: summary.clone(),
            };

            // Stage audit sentinel — emitted outside the lock.
            audit_emits.push(PendingAuditEmit::Approval {
                issue_id: issue_id.clone(),
                plan_id: plan_id.to_string(),
                delegation_id: last_del_id.unwrap_or_default(),
            });

            // Stage beads sync — executes outside the lock.
            if let (Some(closed_status), Some(id)) = (pm_closed_status, issue_id) {
                let comment = format!(
                    "Brain approved: {}",
                    feedback.unwrap_or("meets acceptance criteria")
                );
                let update = spur_pm::IssueUpdate {
                    status: Some(closed_status.to_string()),
                    comment: Some(comment),
                    ..Default::default()
                };
                beads_ops.push(PendingBeadsOp {
                    issue_id: id,
                    update,
                });
            }

            // Dispatch cascade (sync try_send).
            if let (Some(tx), Some(tracker), Some(arc)) =
                (delegation_tx, task_tracker, plan_arc.clone())
            {
                dispatch_newly_ready(
                    plan_id,
                    state,
                    tx,
                    tracker,
                    arc,
                    sink,
                    &mut warnings,
                    &mut new_dispatches,
                    &mut audit_emits,
                    pm_arc,
                );
            }
        }
        "reject" => {
            state.merge_state = PlanMergeState::NotStarted;
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            let issue_id = entry.spec.issue_id.clone();
            let last_del_id = entry.last_delegation_id.clone();
            let feedback_str = feedback.unwrap_or("does not meet requirements");
            entry.status = PlanTaskStatus::Rejected {
                feedback: feedback.map(String::from),
            };

            // Stage audit sentinel — emitted outside the lock.
            audit_emits.push(PendingAuditEmit::Rejection {
                issue_id: issue_id.clone(),
                plan_id: plan_id.to_string(),
                delegation_id: last_del_id.unwrap_or_default(),
                feedback: feedback_str.to_string(),
            });

            // Stage beads sync — executes outside the lock.
            if let Some(id) = issue_id {
                let comment = format!("Brain rejected: {feedback_str}");
                let update = spur_pm::IssueUpdate {
                    status: Some("open".to_string()),
                    comment: Some(comment),
                    ..Default::default()
                };
                beads_ops.push(PendingBeadsOp {
                    issue_id: id,
                    update,
                });
            }

            // Rejection cascade.
            mark_descendants_failed(task_id, state, &mut warnings);
        }
        "request_changes" => {
            state.merge_state = PlanMergeState::NotStarted;
            let fb = feedback.ok_or_else(|| "request_changes requires feedback".to_string())?;

            {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                if entry.attempt >= MAX_ATTEMPTS {
                    let exhausted_fb = format!(
                        "retries exhausted ({}/{}): {}",
                        entry.attempt, MAX_ATTEMPTS, fb
                    );
                    let issue_id = entry.spec.issue_id.clone();
                    let attempt_at_reject = entry.attempt;
                    let last_del_id = entry.last_delegation_id.clone();
                    entry.status = PlanTaskStatus::Rejected {
                        feedback: Some(exhausted_fb.clone()),
                    };
                    warnings.push(format!(
                        "auto-rejected: MAX_ATTEMPTS ({MAX_ATTEMPTS}) reached"
                    ));

                    // Stage audit sentinel — emitted outside the lock.
                    audit_emits.push(PendingAuditEmit::Rejection {
                        issue_id: issue_id.clone(),
                        plan_id: plan_id.to_string(),
                        delegation_id: last_del_id.unwrap_or_default(),
                        feedback: exhausted_fb.clone(),
                    });

                    // Rejection cascade.
                    mark_descendants_failed(task_id, state, &mut warnings);

                    // Stage best-effort beads comment — executes outside the lock.
                    if let Some(id) = issue_id {
                        let comment = format!(
                            "Brain rejected (retries exhausted {}/{}): {}",
                            attempt_at_reject, MAX_ATTEMPTS, fb
                        );
                        let update = spur_pm::IssueUpdate {
                            status: Some("open".to_string()),
                            comment: Some(comment),
                            ..Default::default()
                        };
                        beads_ops.push(PendingBeadsOp {
                            issue_id: id,
                            update,
                        });
                    }

                    let task_name = state
                        .tasks
                        .iter()
                        .find(|t| t.spec.task_id == task_id)
                        .map(|t| display_name(&t.spec.task))
                        .unwrap_or_default();
                    let mut resp = build_plan_status(plan_id, state);
                    if let serde_json::Value::Object(ref mut m) = resp {
                        m.insert("task_id".into(), serde_json::json!(task_id));
                        m.insert("task_name".into(), serde_json::json!(task_name));
                        m.insert("decision".into(), serde_json::json!("reject"));
                        m.insert("warnings".into(), serde_json::json!(warnings));
                    }

                    let events = vec![PendingEvent::TaskReviewed {
                        plan_id: plan_id.to_string(),
                        task_id: task_id.to_string(),
                        task_name: Some(task_name),
                        decision: "reject".to_string(),
                        feedback: Some(exhausted_fb),
                        attempt: attempt_at_reject,
                    }];

                    return Ok(DecisionOutcome {
                        resp,
                        beads_ops,
                        events,
                        audit_emits,
                    });
                }
            }

            // Normal request_changes path.
            let (tx, tracker, arc) = match (delegation_tx, task_tracker, plan_arc.clone()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => {
                    return Err(
                        "request_changes requires orchestrator channel (internal error)"
                            .to_string(),
                    );
                }
            };

            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();

            let current_record = AttemptRecord {
                attempt: entry.attempt,
                worker_branch: entry.worker_branch.clone(),
                diff_summary: entry.result.as_ref().and_then(|r| r.diff_summary.clone()),
                summary: entry.result.as_ref().and_then(|r| r.summary.clone()),
                feedback: fb.to_string(),
            };

            let new_attempt = entry.attempt + 1;

            let mut history_snapshot = entry.history.clone();
            history_snapshot.push(current_record.clone());
            let enriched = build_enriched_task(
                &entry.spec.task,
                &history_snapshot,
                fb,
                new_attempt,
                MAX_ATTEMPTS,
            );

            let delegation_id = uuid::Uuid::new_v4().to_string();
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<DelegationResult>();

            let req = crate::tools::DelegationRequest {
                id: delegation_id.clone().into(),
                agent: entry.spec.agent.clone(),
                task: enriched,
                delegation_plan: None,
                issue_id: entry.spec.issue_id.clone(),
                context_files: entry.spec.context_files.clone(),
                respond_to: resp_tx,
                brain_session_id: state.brain_session_id.clone(),
            };

            // try_send — atomic, sync.
            if let Err(e) = tx.try_send(req) {
                return Err(format!("orchestrator channel error: {e}"));
            }

            entry.worker_branch = None;
            entry.history.push(current_record);
            entry.result = None;
            entry.attempt = new_attempt;
            entry.status = PlanTaskStatus::Dispatched {
                delegation_id: delegation_id.clone(),
            };
            entry.last_delegation_id = Some(delegation_id.clone());

            spawn_completion_future(
                task_id.to_string(),
                delegation_id.clone(),
                resp_rx,
                arc,
                tracker,
                pm_arc.cloned(),
                plan_id.to_string(),
            );

            new_dispatches.push((task_id.to_string(), new_attempt, delegation_id.clone()));

            // Stage Dispatch audit sentinel for the re-dispatched attempt.
            let agent_for_audit = entry.spec.agent.clone();
            let issue_id_for_audit = entry.spec.issue_id.clone();
            audit_emits.push(PendingAuditEmit::Dispatch {
                issue_id: issue_id_for_audit.clone(),
                plan_id: plan_id.to_string(),
                delegation_id: delegation_id.clone(),
                worker: agent_for_audit,
                attempt: new_attempt,
            });

            // Stage beads audit comment — executes outside the lock.
            let superseded_branch: Option<String> =
                entry.history.last().and_then(|h| h.worker_branch.clone());
            if let Some(id) = issue_id_for_audit {
                let comment = format_request_changes_comment(
                    fb,
                    new_attempt - 1,
                    MAX_ATTEMPTS,
                    superseded_branch.as_deref(),
                );
                let update = spur_pm::IssueUpdate {
                    comment: Some(comment),
                    ..Default::default()
                };
                beads_ops.push(PendingBeadsOp {
                    issue_id: id,
                    update,
                });
            }
        }
        other => {
            return Err(format!(
                "invalid decision '{other}': must be 'approve', 'reject', or 'request_changes'"
            ));
        }
    }

    let task_name = state
        .tasks
        .iter()
        .find(|t| t.spec.task_id == task_id)
        .map(|t| display_name(&t.spec.task))
        .unwrap_or_default();

    let mut resp = build_plan_status(plan_id, state);
    if let serde_json::Value::Object(ref mut m) = resp {
        m.insert("task_id".into(), serde_json::json!(task_id));
        m.insert("task_name".into(), serde_json::json!(task_name));
        m.insert("decision".into(), serde_json::json!(decision));
        m.insert("warnings".into(), serde_json::json!(warnings));
        if decision == "request_changes" {
            if let Some((_, new_att, did)) =
                new_dispatches.iter().find(|(tid, _, _)| tid == task_id)
            {
                m.insert("new_attempt".into(), serde_json::json!(new_att));
                m.insert("new_delegation_id".into(), serde_json::json!(did));
                m.insert("max_attempts".into(), serde_json::json!(MAX_ATTEMPTS));
                m.insert(
                    "remaining_attempts".into(),
                    serde_json::json!(MAX_ATTEMPTS.saturating_sub(*new_att)),
                );
            }
        }
    }

    let mut events = vec![PendingEvent::TaskReviewed {
        plan_id: plan_id.to_string(),
        task_id: task_id.to_string(),
        task_name: Some(task_name.clone()),
        decision: decision.to_string(),
        feedback: feedback.map(String::from),
        attempt: current_attempt,
    }];

    for (tid, attempt, did) in &new_dispatches {
        if tid == task_id && decision == "request_changes" {
            events.push(PendingEvent::TaskIterating {
                plan_id: plan_id.to_string(),
                task_id: tid.clone(),
                task_name: Some(task_name.clone()),
                attempt: *attempt,
                delegation_id: did.clone(),
            });
        }
    }

    Ok(DecisionOutcome {
        resp,
        beads_ops,
        events,
        audit_emits,
    })
}

/// Lock-splitting wrapper around `apply_decision_and_extract`.
///
/// The plan lock is held ONLY during the sync state-mutation phase.
/// Beads I/O (`pm.update_issue`) and event emission happen outside the
/// critical section, so concurrent `get_plan_status` / `review_task` calls
/// on the same plan are never blocked by network latency.
#[allow(clippy::too_many_arguments)]
pub async fn handle_review_task(
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    pm: Option<Arc<dyn PmLike>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
) -> Result<serde_json::Value, String> {
    let pm_closed_status = pm.as_deref().map(|p| p.closed_status().to_string());

    // 1) Sync mutation under lock — no .await inside this block.
    let outcome = {
        let mut state = plan_arc.lock().await;
        apply_decision_and_extract(
            plan_id,
            task_id,
            decision,
            feedback,
            &mut state,
            pm_closed_status.as_deref(),
            delegation_tx,
            task_tracker,
            Some(plan_arc.clone()),
            sink,
            pm.as_ref(),
        )
    }?; // lock released here.

    // 2) Async beads I/O — outside the lock.
    if let Some(pm) = pm.as_deref() {
        for op in outcome.beads_ops {
            if let Err(e) = pm.update_issue(&op.issue_id, op.update).await {
                // Beads failures are best-effort; already baked into warnings
                // inside the response if any were anticipated.
                warn!(
                    "handle_review_task: beads update failed for {}: {e}",
                    op.issue_id
                );
            }
        }

        // 2b) Flush audit sentinel emissions — advisory, outside the lock.
        for emit in outcome.audit_emits {
            match emit {
                PendingAuditEmit::Approval {
                    issue_id,
                    plan_id,
                    delegation_id,
                } => {
                    emit_approval_audit(Some(pm), &issue_id, &plan_id, &delegation_id).await;
                }
                PendingAuditEmit::Rejection {
                    issue_id,
                    plan_id,
                    delegation_id,
                    feedback,
                } => {
                    emit_rejection_audit(Some(pm), &issue_id, &plan_id, &delegation_id, &feedback)
                        .await;
                }
                PendingAuditEmit::Dispatch {
                    issue_id,
                    plan_id,
                    delegation_id,
                    worker,
                    attempt,
                } => {
                    emit_dispatch_audit(
                        Some(pm),
                        &issue_id,
                        &plan_id,
                        &delegation_id,
                        &worker,
                        attempt,
                    )
                    .await;
                }
            }
        }
    }

    // 3) Emit events.
    if let Some(sink) = sink {
        for event in outcome.events {
            match event {
                PendingEvent::TaskReviewed {
                    plan_id,
                    task_id,
                    task_name,
                    decision,
                    feedback,
                    attempt,
                } => {
                    sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
                        plan_id,
                        task_id,
                        task_name,
                        decision,
                        feedback,
                        attempt,
                        max_attempts: MAX_ATTEMPTS,
                    });
                }
                PendingEvent::TaskIterating {
                    plan_id,
                    task_id,
                    task_name,
                    attempt,
                    delegation_id,
                } => {
                    sink.emit(spur_acp::SpurEventBody::PlanTaskIterating {
                        plan_id,
                        task_id,
                        task_name,
                        attempt,
                        max_attempts: MAX_ATTEMPTS,
                        delegation_id,
                    });
                }
            }
        }
    }

    Ok(outcome.resp)
}

// ─────────────────────────────────────────────────────────────────────────────

/// BFS from the rejected task through the dependency graph; mark each
/// transitively-dependent task as Failed. Called on reject decisions.
fn mark_descendants_failed(
    rejected_task_id: &str,
    state: &mut PlanState,
    warnings: &mut Vec<String>,
) {
    use std::collections::VecDeque;
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(rejected_task_id.to_string());
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(rejected_task_id.to_string());

    while let Some(parent) = queue.pop_front() {
        // Find all tasks that depend on `parent`.
        let dependents: Vec<String> = state
            .tasks
            .iter()
            .filter(|t| t.spec.depends_on.iter().any(|d| d == &parent))
            .map(|t| t.spec.task_id.clone())
            .collect();

        for dep_id in dependents {
            if !visited.insert(dep_id.clone()) {
                continue;
            }
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == dep_id)
                .unwrap();
            // Only cascade through tasks that haven't already reached a terminal state.
            let should_fail = matches!(
                entry.status,
                PlanTaskStatus::Pending | PlanTaskStatus::Ready
            );
            if should_fail {
                entry.status = PlanTaskStatus::Failed {
                    error: format!("upstream '{parent}' rejected"),
                };
                queue.push_back(dep_id);
            } else {
                warnings.push(format!(
                    "descendant '{dep_id}' not cascaded (already in terminal state)"
                ));
            }
        }
    }
}

/// Scan for Pending tasks whose deps are all Approved; dispatch each.
#[allow(clippy::too_many_arguments)]
fn dispatch_newly_ready(
    plan_id: &str,
    state: &mut PlanState,
    delegation_tx: &tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    task_tracker: &tokio_util::task::TaskTracker,
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    warnings: &mut Vec<String>,
    new_dispatches: &mut Vec<(String, u32, String)>,
    audit_emits: &mut Vec<PendingAuditEmit>,
    pm_arc: Option<&Arc<dyn PmLike>>,
) {
    let ready_ids: Vec<String> = state
        .tasks
        .iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::Pending))
        .filter(|t| {
            t.spec.depends_on.iter().all(|dep| {
                state.tasks.iter().any(|o| {
                    o.spec.task_id == *dep
                        && matches!(
                            o.status,
                            PlanTaskStatus::Approved { .. } | PlanTaskStatus::Cancelled { .. }
                        )
                })
            })
        })
        .map(|t| t.spec.task_id.clone())
        .collect();

    for task_id in ready_ids {
        let (agent, task, issue_id, context_files, brain_session_id) = {
            let e = state
                .tasks
                .iter()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            (
                e.spec.agent.clone(),
                e.spec.task.clone(),
                e.spec.issue_id.clone(),
                e.spec.context_files.clone(),
                state.brain_session_id.clone(),
            )
        };
        let delegation_id = uuid::Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<DelegationResult>();

        let req = crate::tools::DelegationRequest {
            id: delegation_id.clone().into(),
            agent: agent.clone(),
            task,
            delegation_plan: None,
            issue_id: issue_id.clone(),
            context_files,
            respond_to: resp_tx,
            brain_session_id,
        };

        match delegation_tx.try_send(req) {
            Ok(()) => {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                entry.status = PlanTaskStatus::Dispatched {
                    delegation_id: delegation_id.clone(),
                };
                entry.last_delegation_id = Some(delegation_id.clone());
                spawn_completion_future(
                    task_id.clone(),
                    delegation_id.clone(),
                    resp_rx,
                    plan_arc.clone(),
                    task_tracker,
                    pm_arc.cloned(),
                    plan_id.to_string(),
                );
                new_dispatches.push((task_id.clone(), 1, delegation_id.clone()));
                // Stage Dispatch audit sentinel — emitted outside the lock by caller.
                audit_emits.push(PendingAuditEmit::Dispatch {
                    issue_id,
                    plan_id: plan_id.to_string(),
                    delegation_id,
                    worker: agent,
                    attempt: FIRST_DISPATCH_ATTEMPT,
                });
            }
            Err(e) => {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                entry.status = PlanTaskStatus::Failed {
                    error: format!("failed to dispatch: {e}"),
                };
                warnings.push(format!("dispatch failed for '{task_id}': {e}"));
            }
        }
    }
    let _ = sink; // reserved for future event emission on cascade dispatch
}

/// Spawn a future that awaits a DelegationResult and writes it back to PlanState.
/// Guards against stale completions (if the task was iterated again before this
/// resolved, the delegation_id no longer matches — result is discarded).
///
/// After the PlanState mutation under-lock, releases the lock and emits a
/// Completion `[[spur-audit v1]]` sentinel when the result was Success/Modified.
/// Mirrors the emission in `run_plan`'s primary spawn (see mod.rs ~820) so the
/// re-dispatch / approval-cascade paths have the same audit breadcrumb.
fn spawn_completion_future(
    task_id: String,
    expected_delegation_id: String,
    rx: tokio::sync::oneshot::Receiver<DelegationResult>,
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    task_tracker: &tokio_util::task::TaskTracker,
    pm: Option<Arc<dyn PmLike>>,
    plan_id: String,
) {
    task_tracker.spawn(async move {
        let Ok(result) = rx.await else {
            // Oneshot sender dropped — orchestrator died or task cancelled.
            let mut state = plan_arc.lock().await;
            let mut should_cascade = false;
            if let Some(entry) = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id) {
                if let PlanTaskStatus::Dispatched { ref delegation_id } = entry.status {
                    if delegation_id == &expected_delegation_id {
                        entry.status = PlanTaskStatus::Failed {
                            error: "orchestrator channel dropped".to_string(),
                        };
                        should_cascade = true;
                    }
                }
            }
            if should_cascade {
                let mut warnings = Vec::new();
                mark_descendants_failed(&task_id, &mut state, &mut warnings);
            }
            return;
        };

        let mut state = plan_arc.lock().await;
        let Some(entry) = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id) else {
            return;
        };

        // Stale-completion guard: only apply if we're still the expected attempt.
        let still_ours = matches!(
            &entry.status,
            PlanTaskStatus::Dispatched { delegation_id } if delegation_id == &expected_delegation_id
        );
        if !still_ours {
            return;
        }

        let mut transitioned_to_failed = false;
        let mut completion_success = false;
        let mut issue_id_for_audit: Option<String> = None;
        let mut worker_branch_for_audit: Option<String> = None;
        let mut result_summary_for_audit: Option<String> = None;

        match &result.status {
            DelegationStatus::Success | DelegationStatus::Modified { .. } => {
                entry.status = PlanTaskStatus::AwaitingReview {
                    summary: result.summary.clone(),
                };
                entry.worker_branch = result.worker_branch.clone();
                completion_success = true;
                issue_id_for_audit = entry.spec.issue_id.clone();
                worker_branch_for_audit = result.worker_branch.clone();
                result_summary_for_audit = result.summary.clone();
            }
            DelegationStatus::Failed { error } => {
                entry.status = PlanTaskStatus::Failed {
                    error: error.clone(),
                };
                transitioned_to_failed = true;
            }
            DelegationStatus::Cancelled { reason } => {
                entry.status = PlanTaskStatus::Cancelled {
                    reason: reason.clone(),
                };
                // Explicitly DO NOT cascade Cancelled
            }
            other => {
                entry.status = PlanTaskStatus::Failed {
                    error: format!("{other:?}"),
                };
                transitioned_to_failed = true;
            }
        }
        entry.result = Some(result);

        // Cascade organic failures through the dep graph — downstream tasks
        // waiting on this task for Approval will never get it, so mark them
        // Failed too (same as rejection cascade, different trigger).
        if transitioned_to_failed {
            let mut warnings = Vec::new();
            mark_descendants_failed(&task_id, &mut state, &mut warnings);
        }

        // Release lock before audit emission (same discipline as run_plan's
        // primary spawn — beads I/O must not happen under the plan lock).
        drop(state);
        if completion_success {
            emit_completion_audit(
                pm.as_deref(),
                &issue_id_for_audit,
                &plan_id,
                &expected_delegation_id,
                worker_branch_for_audit.as_deref(),
                result_summary_for_audit.as_deref(),
            )
            .await;
        }
    });
}

// ─── Test support ────────────────────────────────────────────────────

/// Utilities for testing `handle_review_task` with injectable pm backends.
#[doc(hidden)]
pub mod test_support {
    use super::PmLike;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    /// A `PmLike` implementation whose `update_issue` fires a signal and then
    /// sleeps for a fixed duration.  The signal lets the test observe that
    /// `update_issue` has been entered (and therefore the plan lock must already
    /// be released) before asserting lock availability.
    pub struct SleepyPm {
        sleep: Duration,
        closed: &'static str,
        /// Fired once, just before the sleep, to signal that `update_issue`
        /// has been entered.  Wrapped in `Mutex<Option<…>>` so it can be taken
        /// from `&self` (trait requires shared ref).
        entered_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[async_trait::async_trait]
    impl PmLike for SleepyPm {
        async fn update_issue(
            &self,
            _id: &str,
            _update: spur_pm::IssueUpdate,
        ) -> anyhow::Result<()> {
            // Signal that we've reached the await point — lock must be free by now.
            if let Some(tx) = self.entered_tx.lock().await.take() {
                let _ = tx.send(());
            }
            tokio::time::sleep(self.sleep).await;
            Ok(())
        }
        fn closed_status(&self) -> &str {
            self.closed
        }
    }

    /// Build a `SleepyPm` with no entry signal.
    pub fn make_sleepy_pm(sleep: Duration) -> Arc<dyn PmLike> {
        Arc::new(SleepyPm {
            sleep,
            closed: "closed",
            entered_tx: Mutex::new(None),
        })
    }

    /// Build a `SleepyPm` that sends `()` on `entered_tx` just before sleeping.
    /// Await the returned receiver before calling `try_lock` to guarantee the
    /// approve task has actually reached `update_issue`'s await point.
    pub fn make_sleepy_pm_with_signal(
        sleep: Duration,
    ) -> (Arc<dyn PmLike>, tokio::sync::oneshot::Receiver<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pm = Arc::new(SleepyPm {
            sleep,
            closed: "closed",
            entered_tx: Mutex::new(Some(tx)),
        });
        (pm, rx)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.into(),
            agent: "test-agent".into(),
            task: "test task".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            context_files: vec![],
        }
    }

    #[test]
    fn is_terminal_plan_status_matches_all_terminal_states() {
        assert!(!super::is_terminal_plan_status("running"));
        assert!(!super::is_terminal_plan_status("awaiting_review"));
        assert!(super::is_terminal_plan_status("approved"));
        assert!(super::is_terminal_plan_status("failed"));
        assert!(super::is_terminal_plan_status("has_failures"));
        assert!(super::is_terminal_plan_status("has_rejections"));
        assert!(super::is_terminal_plan_status("partial"));
        assert!(!super::is_terminal_plan_status("unknown"));
    }

    #[test]
    fn superseded_status_serializes_with_mutation_id() {
        let status = PlanTaskStatus::Superseded {
            mutation_id: "mut-V".into(),
            by: vec!["bd-201".into(), "bd-202".into()],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"status\":\"superseded\""));
        assert!(json.contains("\"mutation_id\":\"mut-V\""));
        assert!(json.contains("\"by\":[\"bd-201\",\"bd-202\"]"));
    }

    #[test]
    fn superseded_is_terminal() {
        assert!(PlanTaskStatus::Superseded {
            mutation_id: "mut-V".into(),
            by: vec![],
        }
        .is_terminal());
    }

    #[test]
    fn valid_linear_plan() {
        let tasks = vec![task("A", &[]), task("B", &["A"]), task("C", &["B"])];
        assert!(validate_plan(&tasks).is_ok());
    }

    #[test]
    fn valid_diamond_plan() {
        let tasks = vec![
            task("A", &[]),
            task("B", &[]),
            task("C", &["A", "B"]),
            task("D", &["C"]),
        ];
        assert!(validate_plan(&tasks).is_ok());
    }

    #[test]
    fn empty_plan_rejected() {
        assert!(validate_plan(&[]).is_err());
    }

    #[test]
    fn duplicate_id_rejected() {
        let tasks = vec![task("A", &[]), task("A", &[])];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("Duplicate"));
    }

    #[test]
    fn dangling_dep_rejected() {
        let tasks = vec![task("A", &["X"])];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("unknown task"));
    }

    #[test]
    fn cycle_rejected() {
        let tasks = vec![task("A", &["B"]), task("B", &["A"])];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("Cycle"));
    }

    #[test]
    fn self_cycle_rejected() {
        let tasks = vec![task("A", &["A"])];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("Cycle"));
    }

    #[test]
    fn three_node_cycle_rejected() {
        let tasks = vec![task("A", &["C"]), task("B", &["A"]), task("C", &["B"])];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("Cycle"));
    }

    #[test]
    fn enriched_task_includes_original_history_and_feedback() {
        let history = vec![super::AttemptRecord {
            attempt: 1,
            worker_branch: Some("spur/worker-x".to_string()),
            diff_summary: None,
            summary: Some("did thing".to_string()),
            feedback: "add null check".to_string(),
        }];
        let enriched = super::build_enriched_task(
            "Implement foo",
            &history,
            "now also handle empty input",
            2,
            super::MAX_ATTEMPTS,
        );
        assert!(enriched.contains("Implement foo"));
        assert!(enriched.contains("Attempt 1"));
        assert!(enriched.contains("add null check"));
        assert!(enriched.contains("now also handle empty input"));
        assert!(enriched.contains("git show"));
        // Retry-budget marker visible to the worker.
        assert!(enriched.contains("Attempt 2 of 3"));
    }

    #[test]
    fn enriched_task_empty_history_omits_previous_attempts_section() {
        // Regression for C5: with an empty history (which is what a caller
        // that forgot to snapshot the current attempt passes), we no longer
        // emit a stray "## Previous Attempts" header. The worker should see
        // only Original + Current Request.
        let enriched = super::build_enriched_task("Task X", &[], "fb", 1, super::MAX_ATTEMPTS);
        assert!(enriched.contains("Task X"));
        assert!(enriched.contains("fb"));
        assert!(!enriched.contains("## Previous Attempts"));
        assert!(enriched.contains("Attempt 1 of 3"));
    }

    #[test]
    fn enriched_task_omits_git_show_hint_when_no_branches() {
        let history = vec![super::AttemptRecord {
            attempt: 1,
            worker_branch: None,
            diff_summary: None,
            summary: Some("s".into()),
            feedback: "fb1".into(),
        }];
        let enriched = super::build_enriched_task("Task", &history, "more", 2, super::MAX_ATTEMPTS);
        assert!(!enriched.contains("git show"));
    }

    #[test]
    fn max_attempts_is_three() {
        assert_eq!(super::MAX_ATTEMPTS, 3);
    }

    #[test]
    fn display_name_trims_and_caps() {
        assert_eq!(super::display_name(""), "");
        assert_eq!(super::display_name("   hello   "), "hello");
        assert_eq!(super::display_name("first line\nsecond"), "first line");
        let long = "x".repeat(100);
        let got = super::display_name(&long);
        assert!(got.ends_with('…'));
        assert!(got.chars().count() <= 61);
    }

    #[test]
    fn display_name_utf8_safe() {
        // Three-byte chars near the boundary — must not panic and must
        // produce valid UTF-8.
        let s = "タスク".repeat(30); // 3 bytes per char × ~90 chars
        let got = super::display_name(&s);
        assert!(got.ends_with('…'));
        assert!(std::str::from_utf8(got.as_bytes()).is_ok());
    }

    // ─── label helper tests ───────────────────────────────────────────

    #[test]
    fn label_value_finds_prefix() {
        let labels = vec![
            "spur:agent:codex".to_string(),
            "priority=high".to_string(),
            "spur:plan-id:custom".to_string(),
        ];
        assert_eq!(super::label_value(&labels, "spur:agent:"), Some("codex"));
        assert_eq!(super::label_value(&labels, "spur:plan-id:"), Some("custom"));
        assert_eq!(super::label_value(&labels, "missing="), None);
    }

    #[test]
    fn strip_spur_labels_drops_machine_prefix() {
        let labels = vec![
            "spur:agent:codex".to_string(),
            "area:auth".to_string(),
            "spur:plan-id:x".to_string(),
            "bug".to_string(),
        ];
        let kept = super::strip_spur_labels(&labels);
        assert_eq!(kept, vec!["area:auth".to_string(), "bug".to_string()]);
    }

    #[test]
    fn dispatch_intent_update_removes_legacy_labels() {
        let update = super::dispatch_intent_update("del-A");
        assert!(update
            .add_labels
            .contains(&super::labels::delegation_id("del-A")));
        assert!(update
            .remove_labels
            .contains(&"delegation-id:del-A".to_string()));
        assert!(update
            .remove_labels
            .contains(&"ready-for-review".to_string()));
    }

    #[test]
    fn immediate_send_failure_compensation_removes_dispatch_label() {
        let update = super::dispatch_send_failure_update("del-A");
        assert!(update
            .remove_labels
            .contains(&super::labels::delegation_id("del-A")));
        assert_eq!(
            update.comment.as_deref(),
            Some("Dispatch send failed before worker ownership was established.")
        );
    }

    #[test]
    fn clear_dispatch_intent_removes_both_namespaced_and_legacy_labels() {
        let update = super::clear_dispatch_intent_update("del-A");
        assert!(update
            .remove_labels
            .contains(&super::labels::delegation_id("del-A")));
        assert!(update
            .remove_labels
            .contains(&"delegation-id:del-A".to_string()));
    }

    // ─── PlanRegistry tests ───────────────────────────────────────────

    #[test]
    fn plan_registry_empty_has_no_entries() {
        let r = super::PlanRegistry::default();
        assert!(r.by_epic.is_empty());
    }

    #[test]
    fn plan_registry_insert_and_lookup() {
        let mut r = super::PlanRegistry::default();
        r.by_epic.insert("bd-100".into(), "plan-abc".into());
        assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-abc".to_string()));
    }

    // ─── derive_epic_plan_from_issues tests ───────────────────────────

    fn make_issue(
        id: &str,
        issue_type: Option<&str>,
        labels: Vec<String>,
        body: &str,
        blocked_by: Vec<String>,
    ) -> spur_pm::Issue {
        use chrono::Utc;
        spur_pm::Issue {
            id: id.to_string(),
            source: spur_pm::PmSource::Beads,
            title: format!("Issue {id}"),
            body: body.to_string(),
            status: "open".to_string(),
            labels,
            assignee: None,
            url: format!("http://beads/issues/{id}"),
            priority: None,
            issue_type: issue_type.map(String::from),
            blocked_by,
            due_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn derive_epic_plan_resolves_agents_and_deps() {
        let epic = make_issue(
            "bd-100",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "Epic body",
            vec![],
        );
        let child_a = make_issue("bd-101", Some("task"), vec![], "Task A body", vec![]);
        let child_b = make_issue(
            "bd-102",
            Some("task"),
            vec![],
            "Task B body",
            vec!["bd-101".to_string()],
        );
        let empty_ext = std::collections::HashMap::new();

        let derived = super::derive_epic_plan_from_issues(
            &epic,
            &[child_a, child_b],
            &empty_ext,
            None,
            &["codex", "claude-code"],
        )
        .unwrap();

        assert_eq!(derived.plan_tasks.len(), 2);
        let a = derived
            .plan_tasks
            .iter()
            .find(|t| t.task_id == "bd-101")
            .unwrap();
        let b = derived
            .plan_tasks
            .iter()
            .find(|t| t.task_id == "bd-102")
            .unwrap();
        assert!(a.depends_on.is_empty());
        assert_eq!(b.depends_on, vec!["bd-101".to_string()]);
        assert_eq!(a.agent, "codex");
        assert_eq!(b.agent, "codex");
        assert_eq!(derived.edge_count, 1);
    }

    #[test]
    fn derive_rejects_non_epic_issue() {
        let epic = make_issue("bd-200", Some("task"), vec![], "body", vec![]);
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap_err();
        assert!(err.contains("not an epic"), "got: {err}");
    }

    #[test]
    fn derive_rejects_empty_children() {
        let epic = make_issue("bd-201", Some("epic"), vec![], "body", vec![]);
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap_err();
        assert!(err.contains("no children"), "got: {err}");
    }

    #[test]
    fn derive_rejects_nested_epic_child() {
        let epic = make_issue("bd-202", Some("epic"), vec![], "body", vec![]);
        let child = make_issue(
            "bd-203",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "sub-epic",
            vec![],
        );
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap_err();
        assert!(err.contains("nested epic child"), "got: {err}");
    }

    #[test]
    fn derive_rejects_unsatisfied_external_dep() {
        let epic = make_issue(
            "bd-204",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "body",
            vec![],
        );
        let child = make_issue(
            "bd-205",
            Some("task"),
            vec![],
            "task body",
            vec!["bd-999".to_string()], // external dep
        );
        let mut ext = std::collections::HashMap::new();
        ext.insert("bd-999".to_string(), "open".to_string());
        let err = super::derive_epic_plan_from_issues(&epic, &[child], &ext, None, &["codex"])
            .unwrap_err();
        assert!(err.contains("external dependency"), "got: {err}");
        assert!(err.contains("not done"), "got: {err}");
    }

    #[test]
    fn derive_allows_done_external_dep() {
        let epic = make_issue(
            "bd-206",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "body",
            vec![],
        );
        let child = make_issue(
            "bd-207",
            Some("task"),
            vec![],
            "task body",
            vec!["bd-999".to_string()], // external dep already done
        );
        let mut ext = std::collections::HashMap::new();
        ext.insert("bd-999".to_string(), "done".to_string());
        let derived =
            super::derive_epic_plan_from_issues(&epic, &[child], &ext, None, &["codex"]).unwrap();
        assert_eq!(derived.plan_tasks.len(), 1);
        assert!(derived.plan_tasks[0].depends_on.is_empty());
        assert!(derived.warnings.iter().any(|w| w.contains("bd-999")));
    }

    #[test]
    fn derive_skips_parent_child_edge_in_blocked_by() {
        // Regression for the real-world bug triggered on bd-1mh: beads
        // flattens the parent-child relationship into the child's
        // blocked_by. The pure function must treat `epic.id` in a child's
        // blocked_by as structural (ignore it), NOT as a missing external
        // dependency. Before this fix, execute_epic on any epic would
        // error with "external dependency '<epic_id>' not done (status=
        // unknown)" because the epic is naturally excluded from the
        // subgraph and naturally absent from external_dep_statuses.
        let epic = make_issue(
            "bd-ep1",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "body",
            vec![],
        );
        let child_a = make_issue(
            "bd-c1",
            Some("task"),
            vec![],
            "task A",
            vec!["bd-ep1".to_string()], // parent-child edge ONLY
        );
        let child_b = make_issue(
            "bd-c2",
            Some("task"),
            vec![],
            "task B",
            // parent-child edge + an intra-subgraph dep on A
            vec!["bd-ep1".to_string(), "bd-c1".to_string()],
        );
        let derived = super::derive_epic_plan_from_issues(
            &epic,
            &[child_a, child_b],
            &std::collections::HashMap::new(), // no external deps needed
            None,
            &["codex"],
        )
        .unwrap();
        assert_eq!(derived.plan_tasks.len(), 2);
        let a = derived
            .plan_tasks
            .iter()
            .find(|t| t.task_id == "bd-c1")
            .unwrap();
        let b = derived
            .plan_tasks
            .iter()
            .find(|t| t.task_id == "bd-c2")
            .unwrap();
        assert!(
            a.depends_on.is_empty(),
            "child A must have no execution deps (parent edge is structural); got {:?}",
            a.depends_on
        );
        assert_eq!(
            b.depends_on,
            vec!["bd-c1".to_string()],
            "child B must depend only on A, not on the epic"
        );
        assert_eq!(derived.edge_count, 1);
    }

    #[test]
    fn derive_inherits_agent_from_epic_label() {
        let epic = make_issue(
            "bd-208",
            Some("epic"),
            vec!["spur:agent:claude-code".to_string()],
            "body",
            vec![],
        );
        // child has NO spur:agent:<name> label
        let child = make_issue("bd-209", Some("task"), vec![], "task body", vec![]);
        let derived = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            None,
            &["codex", "claude-code"],
        )
        .unwrap();
        assert_eq!(derived.plan_tasks[0].agent, "claude-code");
        // should NOT produce a warning (inherited from epic label, not default_agent)
        assert!(derived.warnings.is_empty());
    }

    #[test]
    fn derive_falls_back_to_default_agent() {
        let epic = make_issue("bd-210", Some("epic"), vec![], "body", vec![]);
        let child = make_issue("bd-211", Some("task"), vec![], "task body", vec![]);
        let derived = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            Some("codex"),
            &["codex", "claude-code"],
        )
        .unwrap();
        assert_eq!(derived.plan_tasks[0].agent, "codex");
        // a warning must be emitted when falling back to default_agent
        assert!(
            derived.warnings.iter().any(|w| w.contains("default_agent")),
            "expected default_agent warning, got: {:?}",
            derived.warnings
        );
    }

    #[test]
    fn derive_rejects_missing_agent() {
        let epic = make_issue("bd-212", Some("epic"), vec![], "body", vec![]);
        let child = make_issue("bd-213", Some("task"), vec![], "task body", vec![]);
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            None,
            &["codex", "claude-code"],
        )
        .unwrap_err();
        assert!(err.contains("no agent"), "got: {err}");
        assert!(err.contains("Known agents"), "got: {err}");
    }

    #[test]
    fn derive_rejects_unknown_agent() {
        let epic = make_issue("bd-214", Some("epic"), vec![], "body", vec![]);
        let child = make_issue(
            "bd-215",
            Some("task"),
            vec!["spur:agent:kiro".to_string()],
            "task body",
            vec![],
        );
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            None,
            &["codex", "claude-code"],
        )
        .unwrap_err();
        assert!(err.contains("not configured"), "got: {err}");
        assert!(err.contains("kiro"), "got: {err}");
    }

    #[test]
    fn derive_rejects_empty_agent_label() {
        // A `spur:agent:` label with empty value resolves to ""; must fail
        // the known_agents check with an actionable error.
        let epic = make_issue("bd-230", Some("epic"), vec![], "body", vec![]);
        let child = make_issue(
            "bd-231",
            Some("task"),
            vec!["spur:agent:".to_string()],
            "task body",
            vec![],
        );
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[child],
            &std::collections::HashMap::new(),
            None,
            &["codex", "claude-code"],
        )
        .unwrap_err();
        assert!(err.contains("not configured"), "got: {err}");
    }

    #[test]
    fn derive_cycle_rejected() {
        let epic = make_issue(
            "bd-218",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "body",
            vec![],
        );
        // A depends on B and B depends on A → cycle
        let child_a = make_issue(
            "bd-219",
            Some("task"),
            vec![],
            "task A",
            vec!["bd-220".to_string()],
        );
        let child_b = make_issue(
            "bd-220",
            Some("task"),
            vec![],
            "task B",
            vec!["bd-219".to_string()],
        );
        let err = super::derive_epic_plan_from_issues(
            &epic,
            &[child_a, child_b],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap_err();
        assert!(err.contains("Cycle"), "got: {err}");
    }

    #[test]
    fn rejection_cascade_marks_descendants_failed() {
        use spur_acp::SessionId;
        let tasks = vec![
            super::PlanTask {
                task_id: "A".to_string(),
                agent: "x".to_string(),
                task: "a".to_string(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "B".to_string(),
                agent: "x".to_string(),
                task: "b".to_string(),
                depends_on: vec!["A".to_string()],
                issue_id: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "C".to_string(),
                agent: "x".to_string(),
                task: "c".to_string(),
                depends_on: vec!["B".to_string()],
                issue_id: None,
                context_files: vec![],
            },
        ];
        let mut state = super::PlanState {
            plan_id: "p".to_string(),
            tasks: tasks
                .into_iter()
                .map(|t| super::PlanTaskEntry {
                    spec: t,
                    status: super::PlanTaskStatus::Pending,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                })
                .collect(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            base_snapshot_branch: None,
            merge_state: super::PlanMergeState::NotStarted,
            epic_id: None,
        };
        let mut warnings = Vec::new();
        super::mark_descendants_failed("A", &mut state, &mut warnings);

        // B and C should now be Failed; A remains Pending (caller sets it separately).
        let b = state.tasks.iter().find(|t| t.spec.task_id == "B").unwrap();
        let c = state.tasks.iter().find(|t| t.spec.task_id == "C").unwrap();
        assert!(matches!(b.status, super::PlanTaskStatus::Failed { .. }));
        assert!(matches!(c.status, super::PlanTaskStatus::Failed { .. }));
    }

    // ─── PlanRegistry idempotency tests ──────────────────────────────

    #[test]
    fn registry_tracks_active_plan_per_epic() {
        let mut r = super::PlanRegistry::default();
        r.by_epic.insert("bd-100".into(), "plan-1".into());
        r.by_epic.insert("bd-200".into(), "plan-2".into());
        assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-1".to_string()));
        assert_eq!(r.by_epic.get("bd-200"), Some(&"plan-2".to_string()));
        assert_eq!(r.by_epic.get("bd-999"), None);
    }

    #[test]
    fn registry_entry_replaced_on_reinsert() {
        let mut r = super::PlanRegistry::default();
        r.by_epic.insert("bd-100".into(), "plan-old".into());
        r.by_epic.insert("bd-100".into(), "plan-new".into());
        assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-new".to_string()));
    }

    #[test]
    fn registry_sentinel_constant_is_distinct_from_valid_plan_id() {
        // The reservation sentinel is "__pending__". Verify no real plan_id
        // generation path can collide (UUIDs are hex with hyphens; sentinel
        // contains underscores and 'p' — mutually exclusive).
        let sentinel = "__pending__";
        assert!(sentinel.contains("__"));
        // Sanity: a fresh uuid would never have underscores.
        let uuid = uuid::Uuid::new_v4().to_string();
        assert!(!uuid.contains('_'));
    }

    #[test]
    fn format_request_changes_comment_includes_attempt_feedback_branch() {
        let c = super::format_request_changes_comment(
            "please rename `foo` to `bar`",
            2,
            super::MAX_ATTEMPTS,
            Some("spur/worker-bd-1mh-1"),
        );
        assert!(c.contains("Brain requested changes"));
        assert!(c.contains("attempt 2/3"));
        assert!(c.contains("please rename `foo` to `bar`"));
        assert!(c.contains("spur/worker-bd-1mh-1"));
    }

    #[test]
    fn format_request_changes_comment_reports_reviewed_attempt_not_new() {
        // Scenario: task was at attempt 2, brain request_changes, new_attempt=3.
        // The comment must reference attempt 2 (the one reviewed), not 3.
        // Convention: callers pass `new_attempt - 1` as the attempt arg.
        let reviewed_attempt = 3u32 - 1; // = 2
        let c = super::format_request_changes_comment(
            "fb",
            reviewed_attempt,
            super::MAX_ATTEMPTS,
            None,
        );
        assert!(
            c.contains("attempt 2/3"),
            "comment should show reviewed attempt, got: {c}"
        );
        assert!(
            !c.contains("attempt 3/3"),
            "comment must NOT show new_attempt: {c}"
        );
    }

    #[test]
    fn format_request_changes_comment_no_branch() {
        let c =
            super::format_request_changes_comment("add a null check", 1, super::MAX_ATTEMPTS, None);
        assert!(c.contains("attempt 1/3"));
        assert!(c.contains("add a null check"));
        assert!(c.contains("(no branch yet)"));
    }

    #[tokio::test]
    async fn request_changes_at_max_attempts_auto_rejects() {
        use super::*;
        let task_spec = task("T1", &[]);
        let entry = PlanTaskEntry {
            spec: task_spec,
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("wip".into()),
            },
            result: None,
            worker_branch: None,
            attempt: MAX_ATTEMPTS,
            history: vec![
                AttemptRecord {
                    attempt: 1,
                    worker_branch: Some("spur/worker-1".into()),
                    diff_summary: None,
                    summary: None,
                    feedback: "fix this".into(),
                },
                AttemptRecord {
                    attempt: 2,
                    worker_branch: Some("spur/worker-2".into()),
                    diff_summary: None,
                    summary: None,
                    feedback: "fix that".into(),
                },
            ],
            last_delegation_id: None,
        };
        let mut state = PlanState {
            plan_id: "p1".into(),
            tasks: vec![entry],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
            base_snapshot_branch: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };

        let resp = review_task(
            "p1",
            "T1",
            "request_changes",
            Some("please try the other approach"),
            &mut state,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("should Ok — MAX reached means auto-reject, not Err");

        let entry = &state.tasks[0];
        assert!(
            matches!(entry.status, PlanTaskStatus::Rejected { .. }),
            "expected Rejected at MAX_ATTEMPTS, got {:?}",
            entry.status
        );
        if let PlanTaskStatus::Rejected {
            feedback: Some(ref fb),
        } = entry.status
        {
            assert!(fb.contains("retries exhausted"), "feedback={fb}");
            assert!(fb.contains("3/3"), "feedback={fb}");
            assert!(
                fb.contains("please try the other approach"),
                "feedback={fb}"
            );
        } else {
            panic!("expected Rejected with feedback");
        }

        let obj = resp.as_object().expect("resp is object");
        assert_eq!(obj.get("decision").and_then(|v| v.as_str()), Some("reject"));
        let warnings = obj
            .get("warnings")
            .and_then(|v| v.as_array())
            .expect("warnings array");
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .is_some_and(|s| s.contains("auto-rejected") && s.contains("MAX_ATTEMPTS"))),
            "expected auto-reject warning, got {warnings:?}"
        );

        let overall = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            is_terminal_plan_status(overall),
            "expected terminal overall status, got {overall:?}"
        );
    }

    #[test]
    fn build_task_diff_fields_emits_marker_when_diff_none() {
        let result = spur_acp::DelegationResult {
            diff: None,
            diff_summary: None,
            summary: Some("did work".to_string()),
            status: spur_acp::DelegationStatus::Success,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let fields = super::build_task_diff_fields(&result);
        let m: std::collections::HashMap<String, serde_json::Value> = fields.into_iter().collect();

        assert!(m.contains_key("diff"), "diff key must always be present");
        assert!(
            m["diff"].is_null(),
            "diff value should be null, got {:?}",
            m["diff"]
        );
        assert_eq!(
            m.get("diff_status").and_then(|v| v.as_str()),
            Some("no_changes_detected")
        );
        assert_eq!(
            m.get("diff_basis").and_then(|v| v.as_str()),
            Some("base_commit..HEAD")
        );
        assert_eq!(m.get("summary").and_then(|v| v.as_str()), Some("did work"));
    }

    #[test]
    fn build_task_diff_fields_emits_diff_when_present() {
        let result = spur_acp::DelegationResult {
            diff: Some("diff --git a/x b/x\n...".to_string()),
            diff_summary: None,
            summary: None,
            status: spur_acp::DelegationStatus::Success,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let fields = super::build_task_diff_fields(&result);
        let m: std::collections::HashMap<String, serde_json::Value> = fields.into_iter().collect();

        assert_eq!(
            m.get("diff").and_then(|v| v.as_str()),
            Some("diff --git a/x b/x\n...")
        );
        assert!(!m.contains_key("diff_status"));
        assert!(!m.contains_key("diff_basis"));
    }

    #[test]
    fn build_task_diff_fields_includes_artifact_when_present() {
        use spur_acp::{ArtifactKind, DelegationResult, DelegationStatus, WorkerArtifact};

        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("truncated".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: Some(WorkerArtifact {
                object_ref: "refs/spur/artifacts/xyz".into(),
                blob_sha: "d".repeat(40),
                size_bytes: 99_999,
                kind: ArtifactKind::Output,
            }),
        };
        let fields = super::build_task_diff_fields(&result);
        let map: std::collections::HashMap<String, serde_json::Value> =
            fields.into_iter().collect();
        let art = map
            .get("artifact")
            .expect("artifact field must be surfaced");
        assert_eq!(art["object_ref"], "refs/spur/artifacts/xyz");
        assert_eq!(art["size_bytes"], 99_999);
        assert_eq!(art["kind"], "output");
    }

    #[test]
    fn build_task_diff_fields_omits_artifact_when_absent() {
        use spur_acp::{DelegationResult, DelegationStatus};

        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let fields = super::build_task_diff_fields(&result);
        let map: std::collections::HashMap<String, serde_json::Value> =
            fields.into_iter().collect();
        assert!(!map.contains_key("artifact"));
    }

    #[test]
    fn derive_uses_child_body_as_task_text() {
        // task_text always comes from child.body — no label override.
        let epic = make_issue(
            "bd-250",
            Some("epic"),
            vec!["spur:agent:codex".to_string()],
            "epic body",
            vec![],
        );
        let child_with_body = make_issue("bd-251", Some("task"), vec![], "do the work", vec![]);
        let derived = super::derive_epic_plan_from_issues(
            &epic,
            &[child_with_body],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap();
        assert_eq!(derived.plan_tasks[0].task, "do the work");

        // When child.body is empty, task_text is empty — no fallback.
        let child_empty_body = make_issue("bd-252", Some("task"), vec![], "", vec![]);
        let derived2 = super::derive_epic_plan_from_issues(
            &epic,
            &[child_empty_body],
            &std::collections::HashMap::new(),
            None,
            &["codex"],
        )
        .unwrap();
        assert_eq!(derived2.plan_tasks[0].task, "");
    }

    #[test]
    fn build_plan_status_points_to_merge_plan_before_integration() {
        let state = super::PlanState {
            plan_id: "p-merge".into(),
            tasks: vec![super::PlanTaskEntry {
                spec: super::PlanTask {
                    task_id: "a".into(),
                    agent: "codex".into(),
                    task: "ship it".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: super::PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: Some("spur/worker-a".into()),
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            }],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".into())),
            base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
            merge_state: super::PlanMergeState::NotStarted,
            epic_id: None,
        };

        let status = super::build_plan_status("p-merge", &state);
        assert_eq!(status["status"], "approved");
        assert_eq!(status["ready_to_merge"], true);
        assert!(status["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("merge_plan"));
        assert_eq!(status["merge"]["status"], "not_started");
    }

    #[test]
    fn build_plan_status_points_to_create_pr_after_merge_success() {
        let state = super::PlanState {
            plan_id: "p-merged".into(),
            tasks: vec![super::PlanTaskEntry {
                spec: super::PlanTask {
                    task_id: "a".into(),
                    agent: "codex".into(),
                    task: "ship it".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: super::PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: Some("spur/worker-a".into()),
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            }],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".into())),
            base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
            merge_state: super::PlanMergeState::Succeeded {
                merge_branch: "spur/plan-merge-1".into(),
                merged_task_ids: vec!["a".into()],
            },
            epic_id: None,
        };

        let status = super::build_plan_status("p-merged", &state);
        assert_eq!(status["merge"]["status"], "succeeded");
        assert!(status["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("create_pr"));
        assert_eq!(status["merge"]["merge_branch"], "spur/plan-merge-1");
    }
}
