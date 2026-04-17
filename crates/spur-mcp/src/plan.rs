//! Plan executor — deterministic DAG-based task scheduling.
//!
//! The brain submits a structured plan via `submit_plan`. The executor
//! dispatches tasks to workers in dependency order: tasks with satisfied
//! deps run in parallel, blocked tasks wait. Individual delegations flow
//! through the existing `DelegationRequest` → orchestrator pipeline.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use spur_acp::{DelegationResult, DelegationStatus, SessionId};

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
    Dispatched {
        delegation_id: String,
    },
    /// Worker completed; awaiting brain review.
    AwaitingReview {
        summary: Option<String>,
    },
    /// Brain approved the work.
    Approved {
        summary: Option<String>,
    },
    /// Brain rejected the work.
    Rejected {
        feedback: Option<String>,
    },
    /// Worker failed or dependency failed.
    Failed {
        error: String,
    },
}

/// A task entry in the plan state (spec + runtime status).
#[derive(Debug, Clone)]
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    /// Full delegation result, stored on completion for brain review.
    pub result: Option<DelegationResult>,
    /// Branch the worker committed changes to (populated after AwaitingReview).
    pub worker_branch: Option<String>,
}

/// Runtime state of a submitted plan.
#[derive(Debug)]
pub struct PlanState {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskEntry>,
    pub brain_session_id: SessionId,
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

// ─── Executor ────────────────────────────────────────────────────────

/// Run a submitted plan to completion. Dispatches tasks through the
/// existing delegation channel in dependency order.
///
/// Spawned as a tokio task by `handle_submit_plan`. The plan state is
/// updated in-place; `get_plan_status` reads it concurrently.
pub async fn run_plan(
    plan: Arc<Mutex<PlanState>>,
    delegation_tx: mpsc::Sender<DelegationRequest>,
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
                .filter(|t| matches!(
                    t.status,
                    PlanTaskStatus::AwaitingReview { .. }
                        | PlanTaskStatus::Approved { .. }
                ))
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
                    batch.push((entry.spec.clone(), delegation_id));
                }
            }
            batch
        }; // Lock released.

        for (task_spec, delegation_id) in ready {
            let (tx, rx) = oneshot::channel::<DelegationResult>();

            let request = DelegationRequest {
                id: delegation_id,
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

            let tid = task_spec.task_id.clone();
            let plan_ref = Arc::clone(&plan);
            let pid = plan_id.clone();

            in_flight.spawn(async move {
                match rx.await {
                    Ok(result) => {
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
                                other => {
                                    warn!(plan_id = %pid, task_id = %tid, "Plan task ended: {other:?}");
                                    entry.status = PlanTaskStatus::Failed {
                                        error: format!("{other:?}"),
                                    };
                                }
                            }
                            entry.result = Some(result);
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
            break; // Nothing in flight, nothing to dispatch → done.
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

    // ── Mark unreachable tasks (blocked by failed dependencies) ──────
    {
        let mut p = plan.lock().await;
        let failed_ids: HashSet<String> = p
            .tasks
            .iter()
            .filter(|t| matches!(t.status, PlanTaskStatus::Failed { .. }))
            .map(|t| t.spec.task_id.clone())
            .collect();

        for entry in &mut p.tasks {
            if matches!(entry.status, PlanTaskStatus::Pending) {
                if entry
                    .spec
                    .depends_on
                    .iter()
                    .any(|d| failed_ids.contains(d))
                {
                    entry.status = PlanTaskStatus::Failed {
                        error: "Blocked by failed dependency".into(),
                    };
                }
            }
        }
    }

    let p = plan.lock().await;
    let awaiting_review = p
        .tasks
        .iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::AwaitingReview { .. }))
        .count();
    let approved = p
        .tasks
        .iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::Approved { .. }))
        .count();
    let failed = p
        .tasks
        .iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::Failed { .. }))
        .count();
    info!(
        plan_id = %plan_id,
        total = p.tasks.len(),
        awaiting_review = awaiting_review,
        approved = approved,
        failed = failed,
        "Plan executor finished"
    );
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

    for t in &state.tasks {
        match &t.status {
            PlanTaskStatus::Pending => n_pending += 1,
            PlanTaskStatus::Ready => n_ready += 1,
            PlanTaskStatus::Dispatched { .. } => n_dispatched += 1,
            PlanTaskStatus::AwaitingReview { .. } => n_awaiting_review += 1,
            PlanTaskStatus::Approved { .. } => n_approved += 1,
            PlanTaskStatus::Rejected { .. } => n_rejected += 1,
            PlanTaskStatus::Failed { .. } => n_failed += 1,
        }
    }

    let all_workers_done = n_dispatched == 0 && n_pending == 0 && n_ready == 0;
    let ready_to_merge = all_workers_done && n_awaiting_review == 0 && n_rejected == 0 && n_failed == 0 && n_approved == total;

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
                "agent": t.spec.agent,
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
                                        PlanTaskStatus::AwaitingReview { .. }
                                            | PlanTaskStatus::Approved { .. }
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
                PlanTaskStatus::AwaitingReview { summary } | PlanTaskStatus::Approved { summary } => {
                    let status_str = if matches!(t.status, PlanTaskStatus::AwaitingReview { .. }) {
                        "awaiting_review"
                    } else {
                        "approved"
                    };
                    obj["status"] = status_str.into();
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
                }
                PlanTaskStatus::Failed { error } => {
                    obj["status"] = "failed".into();
                    obj["error"] = error.clone().into();
                }
            }
            obj
        })
        .collect();

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
        "tasks": tasks_json,
    })
}

/// Review a task in a plan: approve or reject, optionally syncing with beads.
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
) -> Result<serde_json::Value, String> {
    let entry = state
        .tasks
        .iter_mut()
        .find(|t| t.spec.task_id == task_id)
        .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;

    let summary = match &entry.status {
        PlanTaskStatus::AwaitingReview { summary } => summary.clone(),
        other => {
            return Err(format!(
                "task '{task_id}' is not awaiting review (current: {other:?})"
            ));
        }
    };

    let mut warnings = Vec::<String>::new();

    match decision {
        "approve" => {
            entry.status = PlanTaskStatus::Approved { summary };
            if let Some(pm) = pm {
                if let Some(issue_id) = entry.spec.issue_id.as_deref() {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::types::IssueUpdate {
                        status: Some("done".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
        }
        "reject" => {
            entry.status = PlanTaskStatus::Rejected {
                feedback: feedback.map(|s| s.to_string()),
            };
            if let Some(pm) = pm {
                if let Some(issue_id) = entry.spec.issue_id.as_deref() {
                    let comment = format!(
                        "Brain rejected: {}",
                        feedback.unwrap_or("does not meet requirements")
                    );
                    let update = spur_pm::types::IssueUpdate {
                        status: Some("open".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
        }
        _ => return Err(format!("invalid decision '{decision}': must be 'approve' or 'reject'")),
    }

    let mut result = build_plan_status(plan_id, state);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("task_id".into(), serde_json::json!(task_id));
        obj.insert("decision".into(), serde_json::json!(decision));
        obj.insert("warnings".into(), serde_json::json!(warnings));
    }
    Ok(result)
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
        let tasks = vec![
            task("A", &["C"]),
            task("B", &["A"]),
            task("C", &["B"]),
        ];
        let err = validate_plan(&tasks).unwrap_err();
        assert!(err.contains("Cycle"));
    }
}
