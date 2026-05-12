//! Plan executor — deterministic DAG-based task scheduling.
//!
//! The brain submits a structured plan via `submit_plan`. The executor
//! dispatches tasks to workers in dependency order: tasks with satisfied
//! deps run in parallel, blocked tasks wait. Individual delegations flow
//! through the existing `DelegationRequest` → orchestrator pipeline.

pub mod audit_sentinel;
pub mod clobber_detector;
pub mod labels;
pub mod mutation;
pub mod mutation_executor;
pub mod outcomes;
pub mod ownership;
pub mod preview;
pub mod projector;
pub mod proposers;
pub mod reconciler;
pub mod scope_snapshot;
pub mod signal_watcher;
pub mod signals;
pub mod snapshot;
pub mod staging;
#[doc(hidden)]
pub mod test_util;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use serde::{Deserialize, Serialize};
use tracing::warn;

use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus};

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
    pub issue_title: Option<String>,
    #[serde(default)]
    pub context_files: Vec<String>,
}

/// Status of an individual plan task.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Setup-time overlay conflict: dispatch could not start because
    /// applying an approved dependency's overlay onto the worker worktree
    /// produced a merge conflict. This is not terminal; the brain can resolve
    /// the upstream conflict and retry.
    BlockedOnSetupConflict {
        dep_task_id: String,
        files: Vec<String>,
    },
    /// bd-2m2u Phase 2d — auto-retry budget (1 attempt) exhausted; brain must drive
    /// recovery via `submit_plan_mutation`. Issue stays open with the
    /// `signal:escalated` label so the engine pauses traversal of the task
    /// without closing the underlying beads issue.
    EscalatedToBrain { last_error: String },
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub worker_branch: Option<String>,
    pub diff_summary: Option<spur_acp::DiffSummary>,
    pub summary: Option<String>,
    /// Brain's `request_changes` feedback that caused this attempt to be superseded.
    pub feedback: String,
    /// HEAD of the worker worktree immediately after overlay cherry-picks
    /// (and before the worker's first commit). Used by `merge_plan` and
    /// `get_task_diff` to compute the worker's net contribution range.
    /// None for legacy attempts dispatched before bd-1dwm Phase 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_base_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_prior_worktree: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AttemptRecordKind {
    #[default]
    BrainRequestedChanges,
    WorkerFailureRecovery,
}

const WORKER_FAILURE_RECOVERY_FEEDBACK_PREFIX: &str =
    "[[spur-attempt-kind:worker-failure-recovery]]\n";

impl AttemptRecord {
    pub fn kind(&self) -> AttemptRecordKind {
        if self
            .feedback
            .starts_with(WORKER_FAILURE_RECOVERY_FEEDBACK_PREFIX)
        {
            AttemptRecordKind::WorkerFailureRecovery
        } else {
            AttemptRecordKind::BrainRequestedChanges
        }
    }

    fn feedback_text(&self) -> &str {
        self.feedback
            .strip_prefix(WORKER_FAILURE_RECOVERY_FEEDBACK_PREFIX)
            .unwrap_or(&self.feedback)
    }
}

pub(crate) fn worker_failure_recovery_feedback(error: &str) -> String {
    format!("{WORKER_FAILURE_RECOVERY_FEEDBACK_PREFIX}{error}")
}

/// A task entry in the plan state (spec + runtime status).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// HEAD of the worker worktree immediately after overlay cherry-picks
    /// for the current (latest) attempt. None for legacy or pre-overlay
    /// dispatches. See bd-1dwm design spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_base_oid: Option<String>,
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

/// Runtime state of a submitted plan.
#[derive(Debug, Clone)]
pub struct PlanState {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskEntry>,
    pub brain_session_id: BrainSessionId,
    /// Shared plan base captured at submission time. `merge_plan` builds the
    /// dedicated integration branch from this ref so merge results are
    /// reproducible and detached from later brain edits.
    pub base_snapshot_branch: Option<String>,
    /// OID matching `base_snapshot_branch` when it was captured. Prefer this
    /// over the branch name when reconstructing restart-time merge/diff state.
    pub base_snapshot_oid: Option<String>,
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

impl PlanState {
    /// Returns plan tasks in topological dependency order. Equal-rank tasks are
    /// ordered by task_id for deterministic staging and dispatch previews.
    pub fn topo_ordered_tasks(&self) -> Vec<&PlanTaskEntry> {
        let id_to_idx: HashMap<&str, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(idx, entry)| (entry.spec.task_id.as_str(), idx))
            .collect();

        let mut in_degree: Vec<usize> = self
            .tasks
            .iter()
            .map(|entry| entry.spec.depends_on.len())
            .collect();
        let mut ready: BTreeSet<&str> = in_degree
            .iter()
            .enumerate()
            .filter_map(|(idx, degree)| {
                if *degree == 0 {
                    Some(self.tasks[idx].spec.task_id.as_str())
                } else {
                    None
                }
            })
            .collect();

        let mut ordered = Vec::with_capacity(self.tasks.len());
        while let Some(task_id) = ready.iter().next().copied() {
            ready.remove(task_id);
            let Some(&idx) = id_to_idx.get(task_id) else {
                continue;
            };
            ordered.push(&self.tasks[idx]);

            for (dependent_idx, dependent) in self.tasks.iter().enumerate() {
                if dependent.spec.depends_on.iter().any(|dep| dep == task_id) {
                    in_degree[dependent_idx] = in_degree[dependent_idx].saturating_sub(1);
                    if in_degree[dependent_idx] == 0 {
                        ready.insert(dependent.spec.task_id.as_str());
                    }
                }
            }
        }

        ordered
    }

    /// Compute the topologically ordered transitive closure of `task_id`'s
    /// dependencies, restricted to currently approved tasks. The entry-point
    /// task itself is never returned.
    pub fn approved_dep_closure(&self, task_id: &str) -> Vec<&PlanTaskEntry> {
        let index_by_task_id = self
            .tasks
            .iter()
            .enumerate()
            .map(|(idx, entry)| (entry.spec.task_id.as_str(), idx))
            .collect::<HashMap<_, _>>();

        if !index_by_task_id.contains_key(task_id) {
            return Vec::new();
        }

        let mut visited = HashSet::new();
        let mut ordered = Vec::new();
        self.dfs_approved_deps(
            task_id,
            task_id,
            &index_by_task_id,
            &mut visited,
            &mut ordered,
        );

        ordered
            .into_iter()
            .map(|idx| &self.tasks[idx])
            .collect::<Vec<_>>()
    }

    fn dfs_approved_deps(
        &self,
        current_task_id: &str,
        entry_task_id: &str,
        index_by_task_id: &HashMap<&str, usize>,
        visited: &mut HashSet<String>,
        ordered: &mut Vec<usize>,
    ) {
        let Some(&current_idx) = index_by_task_id.get(current_task_id) else {
            return;
        };

        for dep_task_id in &self.tasks[current_idx].spec.depends_on {
            let Some(&dep_idx) = index_by_task_id.get(dep_task_id.as_str()) else {
                continue;
            };
            let dep_entry = &self.tasks[dep_idx];
            if !matches!(dep_entry.status, PlanTaskStatus::Approved { .. }) {
                continue;
            }
            if !visited.insert(dep_entry.spec.task_id.clone()) {
                continue;
            }

            self.dfs_approved_deps(
                &dep_entry.spec.task_id,
                entry_task_id,
                index_by_task_id,
                visited,
                ordered,
            );

            if dep_entry.spec.task_id != entry_task_id {
                ordered.push(dep_idx);
            }
        }
    }

    pub fn set_dispatched_base_oid(&mut self, task_id: &str, oid: String) -> bool {
        let Some(entry) = self
            .tasks
            .iter_mut()
            .find(|entry| entry.spec.task_id == task_id)
        else {
            return false;
        };
        entry.dispatched_base_oid = Some(oid);
        true
    }

    /// Build an immutable snapshot for the peer mailbox router. The caller
    /// briefly holds the `PlanState` lock to construct this; afterwards the
    /// snapshot is read without contention.
    pub fn snapshot_for_peer(&self) -> crate::plan::scope_snapshot::PlanScopeSnapshot {
        crate::plan::scope_snapshot::PlanScopeSnapshot {
            plan_version: self.version(),
            peer_edges: self.compute_peer_edges(),
            delegation_to_task: self.delegation_to_task_map(),
            delegation_to_issue: self.delegation_to_issue_map(),
            superseded_tasks: self.superseded_task_ids(),
            terminal_tasks: self.terminal_task_ids(),
        }
    }

    /// Stable, content-addressed plan version. Uses SHA-256 (truncated to
    /// 8 bytes) rather than `DefaultHasher` so the value is reproducible
    /// across Rust upgrades and across binary rebuilds — necessary because
    /// `plan_version` is persisted in `PeerMessageEnvelope` and used by the
    /// router's carry-forward logic to reject stale messages with
    /// `plan_version_changed`. Includes every spec field that affects routing
    /// or downstream task semantics so editing task text bumps the version.
    fn version(&self) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(b"spur-plan-version-v1\0");
        hasher.update(self.plan_id.as_bytes());
        hasher.update(b"\0");
        for entry in &self.tasks {
            hasher.update(entry.spec.task_id.as_bytes());
            hasher.update(b"\0");
            hasher.update(entry.spec.agent.as_bytes());
            hasher.update(b"\0");
            hasher.update(entry.spec.task.as_bytes());
            hasher.update(b"\0");
            for dep in &entry.spec.depends_on {
                hasher.update(dep.as_bytes());
                hasher.update(b"\0");
            }
            hasher.update(b"\x01");
            for ctx in &entry.spec.context_files {
                hasher.update(ctx.as_bytes());
                hasher.update(b"\0");
            }
            hasher.update(b"\x01");
            if let Some(issue) = &entry.spec.issue_id {
                hasher.update(b"i");
                hasher.update(issue.as_bytes());
            }
            hasher.update(b"\0");
            if let Some(last) = &entry.last_delegation_id {
                hasher.update(b"l");
                hasher.update(last.as_bytes());
            }
            hasher.update(b"\0");
            match &entry.status {
                PlanTaskStatus::Pending => hasher.update(b"\x00"),
                PlanTaskStatus::Ready => hasher.update(b"\x01"),
                PlanTaskStatus::Dispatched { delegation_id } => {
                    hasher.update(b"\x02");
                    hasher.update(delegation_id.as_bytes());
                }
                PlanTaskStatus::AwaitingReview { .. } => hasher.update(b"\x03"),
                PlanTaskStatus::Approved { .. } => hasher.update(b"\x04"),
                PlanTaskStatus::Rejected { .. } => hasher.update(b"\x05"),
                PlanTaskStatus::Failed { .. } => hasher.update(b"\x06"),
                PlanTaskStatus::Cancelled { .. } => hasher.update(b"\x07"),
                PlanTaskStatus::Superseded { mutation_id, by } => {
                    hasher.update(b"\x08");
                    hasher.update(mutation_id.as_bytes());
                    hasher.update(b"\0");
                    for child in by {
                        hasher.update(child.as_bytes());
                        hasher.update(b"\0");
                    }
                }
                PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
                    hasher.update(b"\x09");
                    hasher.update(dep_task_id.as_bytes());
                    hasher.update(b"\0");
                    for file in files {
                        hasher.update(file.as_bytes());
                        hasher.update(b"\0");
                    }
                }
                PlanTaskStatus::EscalatedToBrain { last_error } => {
                    hasher.update(b"\x0a");
                    hasher.update(last_error.as_bytes());
                }
            }
            hasher.update(b"\0");
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        u64::from_be_bytes(bytes)
    }

    fn compute_peer_edges(&self) -> HashSet<(String, String)> {
        let task_ids: HashSet<&str> = self.tasks.iter().map(|t| t.spec.task_id.as_str()).collect();
        let mut edges = HashSet::new();
        for entry in &self.tasks {
            for dependency in &entry.spec.depends_on {
                if task_ids.contains(dependency.as_str()) {
                    edges.insert((dependency.clone(), entry.spec.task_id.clone()));
                }
            }
        }
        edges
    }

    fn delegation_to_task_map(
        &self,
    ) -> HashMap<spur_acp::domain::delegation::DelegationId, String> {
        let mut map = HashMap::new();
        for entry in &self.tasks {
            if let Some(delegation_id) = latest_delegation_id(entry) {
                map.insert(
                    spur_acp::domain::delegation::DelegationId(delegation_id.to_string()),
                    entry.spec.task_id.clone(),
                );
            }
        }
        map
    }

    fn delegation_to_issue_map(
        &self,
    ) -> HashMap<spur_acp::domain::delegation::DelegationId, String> {
        let mut map = HashMap::new();
        for entry in &self.tasks {
            if let (Some(delegation_id), Some(issue_id)) =
                (latest_delegation_id(entry), entry.spec.issue_id.as_ref())
            {
                map.insert(
                    spur_acp::domain::delegation::DelegationId(delegation_id.to_string()),
                    issue_id.clone(),
                );
            }
        }
        map
    }

    fn superseded_task_ids(&self) -> HashSet<String> {
        self.tasks
            .iter()
            .filter(|entry| matches!(entry.status, PlanTaskStatus::Superseded { .. }))
            .map(|entry| entry.spec.task_id.clone())
            .collect()
    }

    fn terminal_task_ids(&self) -> HashSet<String> {
        self.tasks
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    PlanTaskStatus::Approved { .. }
                        | PlanTaskStatus::Rejected { .. }
                        | PlanTaskStatus::Failed { .. }
                        | PlanTaskStatus::Cancelled { .. }
                )
            })
            .map(|entry| entry.spec.task_id.clone())
            .collect()
    }
}

/// Returns the most recent delegation id for a task entry, falling back to
/// `last_delegation_id` for non-dispatched statuses. Note: only the latest
/// delegation is exposed; if a task has been retried, prior delegation ids
/// are not returned. Stale-attempt peer messages will resolve to
/// `EdgeCheck::SourceMissing` (not `SourceSuperseded`) at validation time;
/// this is acceptable for v1 because retried tasks get fresh delegation ids
/// and the source worker either emitted before retry (now stale) or after
/// (current).
fn latest_delegation_id(entry: &PlanTaskEntry) -> Option<&str> {
    match &entry.status {
        PlanTaskStatus::Dispatched { delegation_id } => Some(delegation_id.as_str()),
        _ => entry.last_delegation_id.as_deref(),
    }
}

#[cfg(test)]
mod approved_dep_closure_tests {
    use super::*;

    fn entry(id: &str, deps: &[&str], status: PlanTaskStatus) -> PlanTaskEntry {
        PlanTaskEntry {
            spec: PlanTask {
                task_id: id.to_string(),
                agent: "codex".to_string(),
                task: format!("Do {id}"),
                depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
                issue_id: Some(format!("bd-{id}")),
                issue_title: None,
                context_files: Vec::new(),
            },
            status,
            result: None,
            worker_branch: Some(format!("spur/worker-{id}")),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: None,
            dispatched_base_oid: Some(format!("{id}-base")),
        }
    }

    fn state(tasks: Vec<PlanTaskEntry>) -> PlanState {
        PlanState {
            plan_id: "plan-closure".to_string(),
            tasks,
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".to_string())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    fn ids(entries: Vec<&PlanTaskEntry>) -> Vec<&str> {
        entries
            .into_iter()
            .map(|entry| entry.spec.task_id.as_str())
            .collect()
    }

    #[test]
    fn approved_dep_closure_returns_linear_chain_in_topo_order() {
        let state = state(vec![
            entry("M1", &[], PlanTaskStatus::Approved { summary: None }),
            entry("M2", &["M1"], PlanTaskStatus::Approved { summary: None }),
            entry("M3", &["M2"], PlanTaskStatus::Ready),
        ]);

        assert_eq!(ids(state.approved_dep_closure("M3")), vec!["M1", "M2"]);
    }

    #[test]
    fn approved_dep_closure_returns_parallel_siblings_after_shared_root() {
        let state = state(vec![
            entry("root", &[], PlanTaskStatus::Approved { summary: None }),
            entry("M1", &["root"], PlanTaskStatus::Approved { summary: None }),
            entry("M2", &["root"], PlanTaskStatus::Approved { summary: None }),
            entry("M3", &["M1", "M2"], PlanTaskStatus::Ready),
        ]);

        assert_eq!(
            ids(state.approved_dep_closure("M3")),
            vec!["root", "M1", "M2"]
        );
    }

    #[test]
    fn approved_dep_closure_filters_pending_diamond_branch() {
        let state = state(vec![
            entry("root", &[], PlanTaskStatus::Approved { summary: None }),
            entry("M1", &["root"], PlanTaskStatus::Approved { summary: None }),
            entry("M2", &["root"], PlanTaskStatus::Pending),
            entry("M3", &["M1", "M2"], PlanTaskStatus::Ready),
        ]);

        assert_eq!(ids(state.approved_dep_closure("M3")), vec!["root", "M1"]);
    }

    #[test]
    fn topo_ordered_tasks_respects_dependencies() {
        let state = state(vec![
            entry("M3", &["M1", "M2"], PlanTaskStatus::Pending),
            entry("M2", &["root"], PlanTaskStatus::Approved { summary: None }),
            entry("root", &[], PlanTaskStatus::Approved { summary: None }),
            entry("M1", &["root"], PlanTaskStatus::Approved { summary: None }),
        ]);

        assert_eq!(
            ids(state.topo_ordered_tasks()),
            vec!["root", "M1", "M2", "M3"]
        );
    }

    #[test]
    fn topo_ordered_tasks_orders_equal_rank_by_task_id() {
        let state = state(vec![
            entry("C", &[], PlanTaskStatus::Pending),
            entry("A", &[], PlanTaskStatus::Pending),
            entry("B", &[], PlanTaskStatus::Pending),
        ]);

        assert_eq!(ids(state.topo_ordered_tasks()), vec!["A", "B", "C"]);
    }
}

#[cfg(test)]
mod scope_snapshot_integration_tests {
    use super::*;
    use spur_acp::domain::delegation::DelegationId;

    fn task_spec(task_id: &str, agent: &str, deps: Vec<&str>) -> PlanTask {
        PlanTask {
            task_id: task_id.into(),
            agent: agent.into(),
            task: format!("Do {task_id}"),
            depends_on: deps.into_iter().map(String::from).collect(),
            issue_id: Some(format!("bd-{task_id}")),
            issue_title: None,
            context_files: vec![],
        }
    }

    fn entry(
        spec: PlanTask,
        status: PlanTaskStatus,
        last_delegation: Option<&str>,
    ) -> PlanTaskEntry {
        PlanTaskEntry {
            spec,
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: last_delegation.map(String::from),
            dispatched_base_oid: None,
        }
    }

    #[test]
    fn snapshot_for_peer_projects_dag_edges_and_dispatched_delegations() {
        let state = PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![
                entry(
                    task_spec("ta", "codex", vec![]),
                    PlanTaskStatus::Dispatched {
                        delegation_id: "deleg-a".into(),
                    },
                    Some("deleg-a"),
                ),
                entry(
                    task_spec("tb", "kimi", vec!["ta"]),
                    PlanTaskStatus::Dispatched {
                        delegation_id: "deleg-b".into(),
                    },
                    Some("deleg-b"),
                ),
                entry(
                    task_spec("tc", "gemini", vec![]),
                    PlanTaskStatus::Approved { summary: None },
                    Some("deleg-c"),
                ),
            ],
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("bs-1".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };

        let snap = state.snapshot_for_peer();

        // DAG edge: tb depends_on ta.
        assert!(snap
            .peer_edges
            .contains(&("ta".to_string(), "tb".to_string())));
        assert_eq!(snap.peer_edges.len(), 1);

        // Delegations mapped for dispatched + approved tasks.
        assert_eq!(
            snap.delegation_to_task.get(&DelegationId("deleg-a".into())),
            Some(&"ta".to_string())
        );
        assert_eq!(
            snap.delegation_to_task.get(&DelegationId("deleg-b".into())),
            Some(&"tb".to_string())
        );
        assert_eq!(
            snap.delegation_to_task.get(&DelegationId("deleg-c".into())),
            Some(&"tc".to_string())
        );

        // tc is terminal (Approved).
        assert!(snap.terminal_tasks.contains("tc"));
        assert!(!snap.terminal_tasks.contains("ta"));
        assert!(snap.superseded_tasks.is_empty());

        // plan_version is non-zero for non-empty plan.
        assert_ne!(snap.plan_version, 0);
    }

    #[test]
    fn version_is_stable_under_repeated_calls() {
        let state = PlanState {
            plan_id: "plan-stable".into(),
            tasks: vec![entry(
                task_spec("only", "codex", vec![]),
                PlanTaskStatus::Pending,
                None,
            )],
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("bs-1".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };
        let v1 = state.version();
        let v2 = state.version();
        assert_eq!(v1, v2);
    }

    #[test]
    fn version_changes_when_task_text_changes() {
        let mut state = PlanState {
            plan_id: "plan-text".into(),
            tasks: vec![entry(
                task_spec("ta", "codex", vec![]),
                PlanTaskStatus::Pending,
                None,
            )],
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("bs-1".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };
        let v_before = state.version();
        state.tasks[0].spec.task = "Edited task body".into();
        let v_after = state.version();
        assert_ne!(
            v_before, v_after,
            "editing task body must bump plan_version"
        );
    }
}

/// Maximum number of iterations per plan task. After this many attempts,
/// `review_task(request_changes)` returns an error — the brain must approve,
/// reject, or leave the task as-is.
pub const MAX_ATTEMPTS: u32 = 3;
fn should_auto_retry(attempt: u32) -> bool {
    attempt <= 1
}

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
/// the persistent plan engine.
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
        let title = child.title.clone();
        plan_tasks.push(PlanTask {
            task_id: id.clone(),
            agent,
            task: task_text,
            depends_on,
            issue_id: Some(id),
            issue_title: Some(title),
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
    feature_gate: &spur_license::FeatureGate,
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
    let mut derived = derive_epic_plan_from_issues(
        &epic,
        &children,
        &external_dep_statuses,
        default_agent,
        known_agents,
    )?;

    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(crate::server::feature_error_message)?;
    if let Some(adv) = pm.advanced() {
        for task in &mut derived.plan_tasks {
            let Some(issue_id) = task.issue_id.as_deref() else {
                continue;
            };
            let audits = crate::plan::projector::collect_sorted_audits_for_issue(
                issue_id,
                adv.list_comments(issue_id)
                    .await
                    .map_err(|e| format!("failed to list comments for task '{issue_id}': {e}"))?,
            );
            if let Some((_, context_files)) = crate::plan::projector::latest_task_spec(&audits) {
                task.context_files = context_files;
            }
        }
    }

    Ok(derived)
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
pub(crate) fn build_enriched_task(
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
                feedback = rec.feedback_text(),
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

pub fn build_failure_recovery_task(
    original_task: &str,
    history: &[AttemptRecord],
    failure_reason: &str,
    worker_branch: Option<&str>,
    new_attempt: u32,
    max_attempts: u32,
) -> String {
    let mut out = String::with_capacity(original_task.len() + failure_reason.len() + 512);
    out.push_str(original_task);
    out.push_str(&format!(
        "\n\n## Recovery context (Attempt {new_attempt} of {max_attempts})\n\n"
    ));
    out.push_str("The previous attempt(s) failed:\n");

    let mut wrote_current_failure = false;
    for rec in history {
        let branch = rec
            .worker_branch
            .as_deref()
            .or(worker_branch)
            .unwrap_or("(no branch)");
        out.push_str(&format!(
            "- Attempt {attempt}: {error} (branch: {branch})\n",
            attempt = rec.attempt,
            error = rec.feedback_text()
        ));
        if rec.feedback_text() == failure_reason && rec.worker_branch.as_deref() == worker_branch {
            wrote_current_failure = true;
        }
    }

    if history.is_empty() || !wrote_current_failure {
        let failed_attempt = new_attempt.saturating_sub(1).max(1);
        out.push_str(&format!(
            "- Attempt {failed_attempt}: {failure_reason} (branch: {branch})\n",
            branch = worker_branch.unwrap_or("(no branch)")
        ));
    }

    out.push_str(
        "\nInspect the worker branch state with `git log <base>..<branch>`, \
identify what went wrong, and recover from there to complete the original task.\n",
    );
    out
}

pub(crate) fn build_dispatch_task_text(task: &PlanTaskEntry) -> String {
    let Some(last) = task.history.last() else {
        return task.spec.task.clone();
    };
    let new_attempt = last.attempt.saturating_add(1);
    match last.kind() {
        AttemptRecordKind::BrainRequestedChanges => build_enriched_task(
            &task.spec.task,
            &task.history,
            last.feedback_text(),
            new_attempt,
            MAX_ATTEMPTS,
        ),
        AttemptRecordKind::WorkerFailureRecovery => build_failure_recovery_task(
            &task.spec.task,
            &task.history,
            last.feedback_text(),
            last.worker_branch.as_deref(),
            new_attempt,
            MAX_ATTEMPTS,
        ),
    }
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

/// A single auto-injected dependency edge. Returned by `find_sibling_overlaps`
/// and surfaced in the `submit_plan` response so the brain can audit which
/// tasks were serialized.
#[derive(Debug, Clone, Serialize)]
pub struct SiblingOverlap {
    /// Task that must complete first (lex-lower task_id of the pair).
    pub from: String,
    /// Task that gets the synthetic `depends_on: from` edge (lex-higher).
    pub to: String,
    /// The intersection of `context_files` that triggered the synthetic edge.
    pub shared_files: Vec<String>,
}

/// Detect pairs of tasks where:
///   1. Neither is a transitive ancestor of the other (i.e., they could
///      currently dispatch in parallel), AND
///   2. Their `context_files` sets intersect.
///
/// For each such pair, produce one `SiblingOverlap` with `from` = lex-lower
/// task_id, `to` = lex-higher task_id. Determinism matters: callers will
/// inject `depends_on` edges based on this output, and the synthetic graph
/// must not depend on input ordering.
pub fn find_sibling_overlaps(tasks: &[PlanTask]) -> Vec<SiblingOverlap> {
    // Build adjacency (forward edges: dep → dependent) and reachability.
    let id_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();
    let n = tasks.len();
    // Transitive closure via DFS from each node.
    let mut reachable: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for (i, t) in tasks.iter().enumerate() {
        let mut stack: Vec<usize> = t
            .depends_on
            .iter()
            .filter_map(|d| id_to_idx.get(d.as_str()).copied())
            .collect();
        while let Some(node) = stack.pop() {
            if reachable[i].insert(node) {
                for dep in &tasks[node].depends_on {
                    if let Some(&dep_idx) = id_to_idx.get(dep.as_str()) {
                        if !reachable[i].contains(&dep_idx) {
                            stack.push(dep_idx);
                        }
                    }
                }
            }
        }
    }

    let related = |a: usize, b: usize| reachable[a].contains(&b) || reachable[b].contains(&a);

    let mut overlaps = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if related(i, j) {
                continue;
            }
            let files_i: HashSet<&str> =
                tasks[i].context_files.iter().map(String::as_str).collect();
            let shared: Vec<String> = tasks[j]
                .context_files
                .iter()
                .filter(|f| files_i.contains(f.as_str()))
                .cloned()
                .collect();
            if shared.is_empty() {
                continue;
            }
            // Determinism: order pair by lex task_id.
            let (from, to) = if tasks[i].task_id <= tasks[j].task_id {
                (&tasks[i].task_id, &tasks[j].task_id)
            } else {
                (&tasks[j].task_id, &tasks[i].task_id)
            };
            let mut shared_sorted = shared;
            shared_sorted.sort();
            overlaps.push(SiblingOverlap {
                from: from.clone(),
                to: to.clone(),
                shared_files: shared_sorted,
            });
        }
    }
    // Sort by (from, to) for stable output.
    overlaps.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
    overlaps
}

/// Mutates `tasks` in place: for each `SiblingOverlap`, append `from` to the
/// `depends_on` of the task with id `to` (unless already present).
pub fn apply_sibling_overlaps(tasks: &mut [PlanTask], overlaps: &[SiblingOverlap]) {
    if overlaps.is_empty() {
        return;
    }
    let id_to_idx: HashMap<String, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.clone(), i))
        .collect();
    for o in overlaps {
        let Some(&idx) = id_to_idx.get(&o.to) else {
            continue;
        };
        if !tasks[idx].depends_on.iter().any(|d| d == &o.from) {
            tasks[idx].depends_on.push(o.from.clone());
        }
    }
}

/// Submit-time normalization pipeline: validates the plan, computes sibling
/// overlaps, applies synthetic edges, and re-validates (defense-in-depth
/// against any future logic that could introduce cycles). Returns the list
/// of injected overlaps so `submit_plan` can surface them in its response.
#[allow(clippy::ptr_arg)]
pub fn submit_plan_normalize_tasks(
    tasks: &mut Vec<PlanTask>,
) -> Result<Vec<SiblingOverlap>, String> {
    validate_plan(tasks)?;
    let overlaps = find_sibling_overlaps(tasks);
    apply_sibling_overlaps(tasks, &overlaps);
    // Re-validate after mutation. Synthetic edges should never introduce a
    // cycle (lex-ordered pairs are acyclic by construction), but a future
    // refactor could break this — fail loudly if it does.
    validate_plan(tasks).map_err(|e| {
        format!("auto-serialize-siblings produced an invalid plan (this is a bug): {e}")
    })?;
    Ok(overlaps)
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
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    worker: &str,
    attempt: u32,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "dispatch",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Dispatch audit comment emission skipped: {error:?}"
        );
        return;
    }
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

/// Emit a `[[spur-audit v1]] WorkerStarted` sentinel comment on the task issue.
/// Best-effort only: every failure is logged and delegation continues.
#[allow(clippy::too_many_arguments)]
pub async fn emit_worker_started_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    delegation_id: &str,
    worker_branch: &str,
    worker_session_id: &str,
    dispatched_base_oid: &str,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "worker_started",
            issue_id = %issue_id,
            delegation_id = %delegation_id,
            "WorkerStarted audit comment emission skipped: {error:?}"
        );
        return;
    }
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::WorkerStarted {
        delegation_id: delegation_id.to_string(),
        worker_branch: worker_branch.to_string(),
        worker_session_id: worker_session_id.to_string(),
        dispatched_base_oid: dispatched_base_oid.to_string(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "worker_started",
            issue_id = %issue_id,
            delegation_id = %delegation_id,
            worker_branch = %worker_branch,
            worker_session_id = %worker_session_id,
            "WorkerStarted audit comment emission failed: {e}"
        );
    }
}

pub(crate) async fn emit_task_spec_audit(
    advanced: &dyn spur_pm::BeadsAdvanced,
    issue_id: &str,
    task_id: &str,
    context_files: &[String],
) -> anyhow::Result<()> {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::TaskSpec {
        task_id: task_id.to_string(),
        context_files: context_files.to_vec(),
        task_text: None,
        agent: None,
        depends_on: None,
    };
    advanced
        .add_comment(
            issue_id,
            &crate::plan::audit_sentinel::encode_comment(&kind),
        )
        .await?;
    Ok(())
}

/// bd-2m2u Phase 2c — emit a `TaskSpec` audit with extended fields populated by
/// `ModifyTaskSpec`. The projector reads these to override the live beads-issue
/// body / agent label / `blocked_by` set after a brain spec rewrite.
pub(crate) async fn emit_extended_task_spec_audit(
    advanced: &dyn spur_pm::BeadsAdvanced,
    issue_id: &str,
    task_id: &str,
    context_files: &[String],
    task_text: Option<&str>,
    agent: Option<&str>,
    depends_on: Option<&[String]>,
) -> anyhow::Result<()> {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::TaskSpec {
        task_id: task_id.to_string(),
        context_files: context_files.to_vec(),
        task_text: task_text.map(str::to_string),
        agent: agent.map(str::to_string),
        depends_on: depends_on.map(<[String]>::to_vec),
    };
    advanced
        .add_comment(
            issue_id,
            &crate::plan::audit_sentinel::encode_comment(&kind),
        )
        .await?;
    Ok(())
}

/// Emit a `[[spur-audit v1]] Completion` sentinel comment on the task issue.
#[allow(clippy::too_many_arguments)]
pub async fn emit_completion_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    superseded: bool,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
) -> anyhow::Result<()> {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return Ok(());
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "completion",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Completion audit comment emission skipped: {error:?}"
        );
        return Ok(());
    }
    let Some(adv) = pm.advanced() else {
        return Ok(());
    };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::Completion {
        delegation_id: delegation_id.to_string(),
        completion_state,
        superseded,
        worker_branch: fields.worker_branch,
        result_summary: fields.result_summary,
        artifact_uri: fields.artifact_uri,
        dispatched_base_oid: fields.dispatched_base_oid,
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    adv.add_comment(issue_id, &body).await?;
    Ok(())
}

pub async fn emit_epic_completion_audit(
    adv: &dyn spur_pm::BeadsAdvanced,
    epic_id: &str,
    plan_id: &str,
    outcome: crate::plan::audit_sentinel::EpicCompletionOutcome,
) -> anyhow::Result<()> {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::EpicCompletion {
        outcome,
        plan_id: plan_id.to_string(),
        epic_id: epic_id.to_string(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    adv.add_comment(epic_id, &body)
        .await
        .map_err(|error| {
            warn!(
                target: "spur.audit.emit_failure",
                kind = "epic_completion",
                epic_id = %epic_id,
                plan_id = %plan_id,
                "EpicCompletion audit comment emission failed: {error}"
            );
            error
        })
        .map(|_comment_id| ())
}

/// Emit a `[[spur-audit v1]] Approval` sentinel comment on the task issue.
pub(crate) async fn emit_approval_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "approval",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Approval audit comment emission skipped: {error:?}"
        );
        return;
    }
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

async fn apply_issue_update(
    pm: &dyn PmLike,
    issue_id: &str,
    mut update: spur_pm::IssueUpdate,
) -> anyhow::Result<()> {
    let core_update = spur_pm::IssueUpdate {
        status: update.status.take(),
        comment: update.comment.take(),
        priority: update.priority.take(),
        assignee: update.assignee.take(),
        ..Default::default()
    };
    if core_update.status.is_some()
        || core_update.comment.is_some()
        || core_update.priority.is_some()
        || core_update.assignee.is_some()
    {
        pm.update_issue(issue_id, core_update).await?;
    }

    if !update.add_labels.is_empty() || !update.remove_labels.is_empty() {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: update.add_labels,
                remove_labels: update.remove_labels,
                ..Default::default()
            },
        )
        .await?;
    }

    Ok(())
}

/// Emit a `[[spur-audit v1]] Rejection` sentinel comment on the task issue.
pub(crate) async fn emit_rejection_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    feedback: &str,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "rejection",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "Rejection audit comment emission skipped: {error:?}"
        );
        return;
    }
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

/// Emit a `[[spur-audit v1]] ReviewFeedback` sentinel comment so the projector
/// can rebuild `AttemptRecord.history` from beads on every reprojection.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_review_feedback_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    attempt: u32,
    feedback: &str,
    worker_branch: Option<String>,
    summary: Option<String>,
    reuse_prior_worktree: Option<bool>,
) {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return;
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "review_feedback",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "ReviewFeedback audit comment emission skipped: {error:?}"
        );
        return;
    }
    let Some(adv) = pm.advanced() else { return };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::ReviewFeedback {
        delegation_id: delegation_id.to_string(),
        attempt,
        feedback: feedback.to_string(),
        worker_branch,
        summary,
        reuse_prior_worktree,
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = adv.add_comment(issue_id, &body).await {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "review_feedback",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "ReviewFeedback audit comment emission failed: {e}"
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_retry_requested_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    attempt: u32,
    error: &str,
    worker_branch: Option<String>,
    amended_prompt_summary: Option<String>,
) -> anyhow::Result<()> {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return Ok(());
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "retry_requested",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "RetryRequested audit comment emission skipped: {error:?}"
        );
        return Ok(());
    }
    let Some(adv) = pm.advanced() else {
        return Ok(());
    };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::RetryRequested {
        delegation_id: delegation_id.to_string(),
        attempt,
        error: error.to_string(),
        worker_branch,
        amended_prompt_summary,
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    adv.add_comment(issue_id, &body).await?;
    Ok(())
}

/// bd-2m2u Phase 2d — emit `EscalationRequested` audit on the task's issue.
/// Mirrors `emit_retry_requested_audit`. `task_id` may be empty when the
/// caller cannot resolve a stable plan task id — the projector / brain
/// observer can still scope the audit by `plan_id` + `issue_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_escalation_requested_audit(
    pm: Option<&dyn PmLike>,
    issue_id: &Option<String>,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    task_id: &str,
    delegation_id: &str,
    attempt: u32,
    last_error: &str,
    worker_branch: Option<String>,
) -> anyhow::Result<()> {
    let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) else {
        return Ok(());
    };
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        warn!(
            target: "spur.audit.emit_failure",
            kind = "escalation_requested",
            issue_id = %issue_id,
            plan_id = %plan_id,
            delegation_id = %delegation_id,
            "EscalationRequested audit comment emission skipped: {error:?}"
        );
        return Ok(());
    }
    let Some(adv) = pm.advanced() else {
        return Ok(());
    };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::EscalationRequested {
        plan_id: plan_id.to_string(),
        task_id: task_id.to_string(),
        attempt,
        last_error: last_error.to_string(),
        worker_branch,
        delegation_id: Some(delegation_id.to_string()),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    adv.add_comment(issue_id, &body).await?;
    Ok(())
}

/// Build the persisted label mutation used when a task is about to be sent
/// to a worker.
pub fn dispatch_intent_update(
    delegation_id: &str,
    lease_expires_at: i64,
    current_labels: &[String],
) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        add_labels: vec![
            crate::plan::labels::delegation_id(delegation_id),
            crate::plan::labels::lease_expires_at(lease_expires_at),
        ],
        remove_labels: vec![
            format!("delegation-id:{delegation_id}"),
            crate::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
        ]
        .into_iter()
        .chain(lease_label_removals(current_labels, Some(lease_expires_at)))
        .collect(),
        ..Default::default()
    }
}

/// Persist dispatch intent before the worker send happens.
#[allow(clippy::too_many_arguments)]
pub async fn persist_dispatch_intent(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    worker: &str,
    attempt: u32,
    lease_duration: Duration,
) -> anyhow::Result<()> {
    let lease_expires_at = chrono::Utc::now()
        .timestamp()
        .saturating_add(i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX));
    let current_labels = pm.issue_labels(issue_id).await?;
    apply_issue_update(
        pm,
        issue_id,
        dispatch_intent_update(delegation_id, lease_expires_at, &current_labels),
    )
    .await?;
    emit_dispatch_audit(
        Some(pm),
        &Some(issue_id.to_string()),
        feature_gate,
        plan_id,
        delegation_id,
        worker,
        attempt,
    )
    .await;
    Ok(())
}

/// Build the compensating mutation when a worker send fails immediately.
pub fn dispatch_send_failure_update(
    delegation_id: &str,
    current_labels: &[String],
) -> spur_pm::IssueUpdate {
    let mut update = clear_dispatch_intent_update(delegation_id, current_labels);
    update.comment =
        Some("Dispatch send failed before worker ownership was established.".to_string());
    update
}

/// Build the mutation used when dispatch intent is no longer active.
pub fn clear_dispatch_intent_update(
    delegation_id: &str,
    current_labels: &[String],
) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        remove_labels: vec![
            crate::plan::labels::delegation_id(delegation_id),
            format!("delegation-id:{delegation_id}"),
        ]
        .into_iter()
        .chain(lease_label_removals(current_labels, None))
        .collect(),
        ..Default::default()
    }
}

/// Clear dispatch intent on the backing PM record.
pub async fn clear_dispatch_intent(
    pm: &dyn PmLike,
    issue_id: &str,
    delegation_id: &str,
) -> anyhow::Result<()> {
    let current_labels = pm.issue_labels(issue_id).await?;
    apply_issue_update(
        pm,
        issue_id,
        clear_dispatch_intent_update(delegation_id, &current_labels),
    )
    .await
}

fn lease_label_removals(current_labels: &[String], keep_expires_at: Option<i64>) -> Vec<String> {
    current_labels
        .iter()
        .filter(|label| {
            crate::plan::labels::parse_lease_expires_at(label)
                .is_some_and(|expires_at| keep_expires_at.is_none_or(|keep| keep != expires_at))
        })
        .cloned()
        .collect()
}

/// Re-stamp the dispatch lease.
///
/// The PM label mutation can expose a transient dual-lease window during its
/// add -> remove sequence: a reader may see both the old and new
/// `spur:lease-expires-at:*` labels. The GC sweep intentionally folds those
/// labels with `.max()` so that the newest heartbeat wins during that window.
/// Repeated calls with the same timestamp are idempotent.
pub async fn update_dispatch_lease(
    pm: &dyn PmLike,
    issue_id: &str,
    _delegation_id: &str,
    new_expires_at: i64,
) -> anyhow::Result<()> {
    let current_labels = pm.issue_labels(issue_id).await?;
    apply_issue_update(
        pm,
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::lease_expires_at(new_expires_at)],
            remove_labels: lease_label_removals(&current_labels, Some(new_expires_at)),
            ..Default::default()
        },
    )
    .await
}

pub fn completion_success_update() -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        add_labels: vec![crate::plan::labels::READY_FOR_REVIEW.to_string()],
        ..Default::default()
    }
}

pub fn completion_terminal_update(closed_status: &str) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        status: Some(closed_status.to_string()),
        remove_labels: vec![
            crate::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
        ],
        ..Default::default()
    }
}

fn completion_retry_update() -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        status: Some("open".to_string()),
        remove_labels: review_ready_label_removals(),
        ..Default::default()
    }
}

/// bd-2m2u Phase 2d — keeps the beads issue OPEN, removes the
/// `READY_FOR_REVIEW` label so the SignalWatcher does NOT pick it up
/// (option A routing), and adds `signal:escalated` so brain tooling /
/// `submit_plan_mutation` can clear it on resolution.
fn completion_escalation_update() -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        status: Some("open".to_string()),
        add_labels: vec![crate::plan::mutation_executor::SIGNAL_ESCALATED_LABEL.to_string()],
        remove_labels: review_ready_label_removals(),
        ..Default::default()
    }
}

fn review_ready_label_removals() -> Vec<String> {
    vec![
        crate::plan::labels::READY_FOR_REVIEW.to_string(),
        "ready-for-review".to_string(),
    ]
}

fn approve_review_update(closed_status: &str, comment: String) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        status: Some(closed_status.to_string()),
        comment: Some(comment),
        remove_labels: review_ready_label_removals(),
        ..Default::default()
    }
}

fn reject_review_update(closed_status: &str, comment: String) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        status: Some(closed_status.to_string()),
        comment: Some(comment),
        add_labels: vec![crate::plan::labels::REVIEW_REJECTED.to_string()],
        remove_labels: review_ready_label_removals(),
        ..Default::default()
    }
}

pub fn completion_is_superseded(
    delegation_id: &str,
    audits: &[crate::plan::audit_sentinel::AuditSentinelKind],
) -> bool {
    audits.iter().any(|audit| {
        matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: cleared,
                ..
            } if cleared == delegation_id
        )
    })
}

/// True if a `Completion` audit sentinel already exists for this delegation_id.
/// Mirrors `completion_is_superseded` shape — both ask "what does PM truth say?"
pub fn completion_audit_already_emitted(
    delegation_id: &str,
    audits: &[crate::plan::audit_sentinel::AuditSentinelKind],
) -> bool {
    audits.iter().any(|audit| {
        matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                delegation_id: emitted,
                ..
            } if emitted == delegation_id
        )
    })
}

fn log_worker_output_commit_count(
    completion_state: &crate::plan::audit_sentinel::CompletionState,
    fields: &crate::plan::audit_sentinel::CompletionAuditFields,
) {
    use crate::plan::audit_sentinel::CompletionState;

    if completion_state != &CompletionState::AwaitingReview {
        return;
    }

    let (Some(base), Some(branch)) = (
        fields.dispatched_base_oid.as_deref(),
        fields.worker_branch.as_deref(),
    ) else {
        return;
    };

    let repo_root = fields
        .repo_root
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("."));
    match crate::plan::audit_sentinel::count_worker_commits(repo_root, base, branch) {
        Ok(count) => tracing::info!(
            branch,
            base,
            count,
            "observed worker output commit count at completion"
        ),
        Err(error) => tracing::warn!(
            branch,
            base,
            "worker output commit count observation failed: {error}"
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompletionPersistenceAction {
    Completed(crate::plan::audit_sentinel::CompletionState),
    AlreadyCompleted,
    AutoRetried {
        error: String,
        worker_branch: Option<String>,
    },
    /// bd-2m2u Phase 2d — auto-retry budget (1 attempt) exhausted; the issue is kept
    /// open with `signal:escalated`, an `EscalationRequested` audit is
    /// emitted, and the caller pushes a `BrainContinuation` with
    /// `ContinuationSource::PlanTaskEscalated`.
    Escalated {
        last_error: String,
        worker_branch: Option<String>,
    },
}

#[allow(clippy::too_many_arguments)]
async fn persist_completion_result_after_worker_output_invariant(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
    already_emitted: bool,
) -> anyhow::Result<()> {
    log_worker_output_commit_count(&completion_state, &fields);
    persist_completion_result(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        fields,
        already_emitted,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_completion_result_after_worker_output_invariant_with_retry(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
    already_emitted: bool,
    attempt: u32,
    task_id: Option<&str>,
) -> anyhow::Result<CompletionPersistenceAction> {
    log_worker_output_commit_count(&completion_state, &fields);
    persist_completion_result_with_retry_for_task(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        fields,
        already_emitted,
        Some(attempt),
        task_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_completion_result(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
    already_emitted: bool,
) -> anyhow::Result<()> {
    persist_completion_result_with_retry(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        fields,
        already_emitted,
        None,
    )
    .await
    .map(|_| ())
}

/// Persisted worker-failure chokepoint. All persisted completion writers flow
/// through this function after worker-output invariant checks, so the
/// auto-retry decision lives here to emit `Completion` and `RetryRequested`
/// audits consistently.
#[allow(clippy::too_many_arguments)]
async fn persist_completion_result_with_retry(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
    already_emitted: bool,
    retry_attempt: Option<u32>,
) -> anyhow::Result<CompletionPersistenceAction> {
    persist_completion_result_with_retry_for_task(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        fields,
        already_emitted,
        retry_attempt,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_completion_result_with_retry_for_task(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fields: crate::plan::audit_sentinel::CompletionAuditFields,
    already_emitted: bool,
    retry_attempt: Option<u32>,
    task_id: Option<&str>,
) -> anyhow::Result<CompletionPersistenceAction> {
    use crate::plan::audit_sentinel::CompletionState;

    let auto_retry =
        completion_state == CompletionState::Failed && retry_attempt.is_some_and(should_auto_retry);
    // bd-2m2u Phase 2d — if retry budget is exhausted, promote to escalation
    // instead of terminal-Failed. `should_escalate(attempt) = !should_auto_retry(attempt)`.
    let escalate = completion_state == CompletionState::Failed
        && retry_attempt.is_some_and(|attempt| !should_auto_retry(attempt));
    let failure_message = || {
        fields
            .result_summary
            .clone()
            .unwrap_or_else(|| "worker failed".to_string())
    };
    let retry_error = auto_retry.then(failure_message);
    let retry_worker_branch = auto_retry.then(|| fields.worker_branch.clone()).flatten();
    let escalation_error = escalate.then(failure_message);
    let escalation_worker_branch = escalate.then(|| fields.worker_branch.clone()).flatten();

    if !already_emitted && completion_state != CompletionState::Superseded {
        if completion_state == CompletionState::AwaitingReview
            && crate::server::require_feature(
                spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
                feature_gate,
            )
            .is_ok()
        {
            if let Some(adv) = pm.advanced() {
                let current_audits = crate::plan::projector::collect_sorted_audits_for_issue(
                    issue_id,
                    adv.list_comments(issue_id).await?,
                );
                if completion_audit_already_emitted(delegation_id, &current_audits) {
                    return Ok(CompletionPersistenceAction::AlreadyCompleted);
                }
            }
        }
        emit_completion_audit(
            Some(pm),
            &Some(issue_id.to_string()),
            feature_gate,
            plan_id,
            delegation_id,
            completion_state,
            false,
            fields,
        )
        .await?;
    } else if completion_state == CompletionState::Superseded && !already_emitted {
        emit_completion_audit(
            Some(pm),
            &Some(issue_id.to_string()),
            feature_gate,
            plan_id,
            delegation_id,
            completion_state,
            true,
            fields,
        )
        .await?;
    }

    if let (Some(attempt), Some(error)) = (retry_attempt, retry_error.as_deref()) {
        let amended_prompt_summary = Some(format!(
            "Attempt {} recovery: {}{}",
            attempt,
            error,
            retry_worker_branch
                .as_deref()
                .map(|b| format!(" (branch: {b})"))
                .unwrap_or_default()
        ));
        emit_retry_requested_audit(
            Some(pm),
            &Some(issue_id.to_string()),
            feature_gate,
            plan_id,
            delegation_id,
            attempt,
            error,
            retry_worker_branch.clone(),
            amended_prompt_summary,
        )
        .await?;
    }

    if let (Some(attempt), Some(error)) = (retry_attempt, escalation_error.as_deref()) {
        emit_escalation_requested_audit(
            Some(pm),
            &Some(issue_id.to_string()),
            feature_gate,
            plan_id,
            task_id.unwrap_or(""),
            delegation_id,
            attempt,
            error,
            escalation_worker_branch.clone(),
        )
        .await?;
    }

    let current_labels = pm.issue_labels(issue_id).await?;
    let mut update = match completion_state {
        CompletionState::AwaitingReview => completion_success_update(),
        CompletionState::Failed if auto_retry => completion_retry_update(),
        CompletionState::Failed if escalate => completion_escalation_update(),
        CompletionState::Failed | CompletionState::Cancelled => {
            completion_terminal_update(pm.closed_status())
        }
        CompletionState::Superseded => spur_pm::IssueUpdate::default(),
    };
    let clear = clear_dispatch_intent_update(delegation_id, &current_labels);
    update.remove_labels.extend(clear.remove_labels);
    apply_issue_update(pm, issue_id, update).await?;
    Ok(if let Some(error) = retry_error {
        CompletionPersistenceAction::AutoRetried {
            error,
            worker_branch: retry_worker_branch,
        }
    } else if let Some(last_error) = escalation_error {
        CompletionPersistenceAction::Escalated {
            last_error,
            worker_branch: escalation_worker_branch,
        }
    } else {
        CompletionPersistenceAction::Completed(completion_state)
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_worker_completion_and_notify(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    fast_forward: &Option<Arc<tokio::sync::Notify>>,
    result: &DelegationResult,
    brain_session_id: &BrainSessionId,
    attempt: u32,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
    dispatched_base_oid: Option<String>,
    task_id: Option<&str>,
) -> anyhow::Result<Option<DeferredCompletionPush>> {
    let (completion_state, audits) =
        derive_worker_completion_state(pm, feature_gate, issue_id, delegation_id, &result.status)
            .await?;
    let already_emitted = audits
        .as_deref()
        .map(|audits| completion_audit_already_emitted(delegation_id, audits))
        .unwrap_or(false);
    persist_completion_inner(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        already_emitted,
        fast_forward,
        result,
        brain_session_id,
        attempt,
        materializer,
        dispatched_base_oid,
        None,
        task_id,
    )
    .await
}

/// **SYSTEM PATH ONLY — do not call from worker-completion sites.**
///
/// This entry point is for system-authoritative writers (today: only the lease
/// GC at `reconciler::sweep_expired_dispatch_leases`). It bypasses the
/// `completion_is_superseded` check that worker callers go through, because
/// the GC just wrote the very `DispatchOrphanCleared` audit a supersede check
/// would query — running the check would convert the GC's own authoritative
/// `Failed` into `Superseded` and prevent the task from closing.
///
/// In practice the system-path caller always passes `CompletionState::Failed`
/// (or `Cancelled`). The signature accepts any `CompletionState` for
/// flexibility, but worker-path callers MUST use
/// `persist_worker_completion_and_notify` instead — it derives the state
/// (including the supersede check) internally.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_system_completion_and_notify(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fast_forward: &Option<Arc<tokio::sync::Notify>>,
    result: &DelegationResult,
    brain_session_id: &BrainSessionId,
    attempt: u32,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
    dispatched_base_oid: Option<String>,
    repo_root: Option<std::path::PathBuf>,
    task_id: Option<&str>,
) -> anyhow::Result<Option<DeferredCompletionPush>> {
    let audits = read_audits_if_advanced(pm, feature_gate, issue_id).await?;
    let already_emitted = audits
        .as_deref()
        .map(|audits| completion_audit_already_emitted(delegation_id, audits))
        .unwrap_or(false);
    persist_completion_inner(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        already_emitted,
        fast_forward,
        result,
        brain_session_id,
        attempt,
        materializer,
        dispatched_base_oid,
        repo_root,
        task_id,
    )
    .await
}

/// Derive the `CompletionState` a worker's late completion should land at,
/// downgrading to `Superseded` when a `DispatchOrphanCleared` audit already
/// exists for this `delegation_id` (Race A: GC reclaimed the lease before the
/// worker's result arrived).
///
/// **Graceful unlicensed fallback.** If `pm.advanced()` is `None` (non-beads
/// backend) or `PM_PRO_BEADS_ADVANCED` is not licensed on the feature gate,
/// returns `Ok((baseline, None))` without propagating the gate error. This is
/// the same "fail-open to baseline" contract used by
/// `issue_has_plan_pending_sweep_comment` in bd-6okx.2 attempt #3, and is safe
/// today because `resolve_dispatch_orphan` is gated on the same feature key —
/// an unlicensed system cannot produce `DispatchOrphanCleared` audits in the
/// first place, so the baseline matches reality.
///
/// Edge case to revisit: if the license is downgraded mid-delegation
/// (Pro→Free between dispatch and completion), a late worker `Success` could
/// land without being superseded. Acceptable today because mid-delegation
/// license downgrade is rare and orphan-clear-on-community-edition is not yet
/// a supported path.
async fn derive_worker_completion_state(
    pm: &dyn PmLike,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
    delegation_id: &str,
    status: &DelegationStatus,
) -> anyhow::Result<(
    crate::plan::audit_sentinel::CompletionState,
    Option<Vec<crate::plan::audit_sentinel::AuditSentinelKind>>,
)> {
    let baseline = completion_state_from_status(status);
    if crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .is_err()
    {
        return Ok((baseline, None));
    }
    let Some(adv) = pm.advanced() else {
        return Ok((baseline, None));
    };

    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        issue_id,
        adv.list_comments(issue_id).await?,
    );
    let state = if completion_is_superseded(delegation_id, &audits) {
        crate::plan::audit_sentinel::CompletionState::Superseded
    } else {
        baseline
    };
    Ok((state, Some(audits)))
}

/// Read PM-truth audits for the system path (lease GC, manual close, etc.).
/// Returns None on unlicensed/no-advanced (matches derive_worker_completion_state shape).
async fn read_audits_if_advanced(
    pm: &dyn PmLike,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
) -> anyhow::Result<Option<Vec<crate::plan::audit_sentinel::AuditSentinelKind>>> {
    if crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .is_err()
    {
        return Ok(None);
    }
    let Some(adv) = pm.advanced() else {
        return Ok(None);
    };
    Ok(Some(
        crate::plan::projector::collect_sorted_audits_for_issue(
            issue_id,
            adv.list_comments(issue_id).await?,
        ),
    ))
}

#[allow(clippy::too_many_arguments)]
async fn persist_completion_inner(
    pm: &dyn PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    already_emitted: bool,
    fast_forward: &Option<Arc<tokio::sync::Notify>>,
    result: &DelegationResult,
    brain_session_id: &BrainSessionId,
    attempt: u32,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
    dispatched_base_oid: Option<String>,
    repo_root: Option<std::path::PathBuf>,
    task_id: Option<&str>,
) -> anyhow::Result<Option<DeferredCompletionPush>> {
    use crate::plan::audit_sentinel::CompletionState;

    if matches!(completion_state, CompletionState::Superseded) {
        persist_completion_result_after_worker_output_invariant(
            pm,
            issue_id,
            feature_gate,
            plan_id,
            delegation_id,
            completion_state,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: result.worker_branch.clone(),
                result_summary: result.summary.clone(),
                artifact_uri: None,
                dispatched_base_oid,
                repo_root,
            },
            already_emitted,
        )
        .await?;
        crate::server::notify_fast_forward(fast_forward);
        return Ok(None);
    }

    let source = match completion_state {
        CompletionState::AwaitingReview => {
            spur_acp::domain::ContinuationSource::PlanTaskAwaitingReview
        }
        CompletionState::Failed => spur_acp::domain::ContinuationSource::PlanTaskFailed,
        CompletionState::Cancelled => spur_acp::domain::ContinuationSource::Cancelled,
        CompletionState::Superseded => unreachable!("handled above"),
    };
    let cont = materializer
        .materialize(
            result.clone(),
            spur_acp::DelegationId::from(delegation_id),
            attempt,
            brain_session_id.clone(),
            source.clone(),
            None,
        )
        .await;
    let artifact_uri = cont.payload.artifact_id.as_ref().map(|key| {
        format!(
            "spur://outcome/{}/{}/{}",
            key.brain_session_id.as_session_id().0,
            key.delegation_id.as_str(),
            key.attempt
        )
    });

    let persistence_action = persist_completion_result_after_worker_output_invariant_with_retry(
        pm,
        issue_id,
        feature_gate,
        plan_id,
        delegation_id,
        completion_state,
        crate::plan::audit_sentinel::CompletionAuditFields {
            worker_branch: result.worker_branch.clone(),
            result_summary: cont
                .payload
                .summary
                .clone()
                .or_else(|| failure_reason_from_status(&result.status)),
            artifact_uri,
            dispatched_base_oid,
            repo_root,
        },
        already_emitted,
        attempt,
        task_id,
    )
    .await?;
    crate::server::notify_fast_forward(fast_forward);

    let completion_state = match persistence_action {
        CompletionPersistenceAction::AutoRetried {
            error,
            worker_branch,
        } => {
            let event = task_id.map(|tid| {
                PlanTaskNotificationEventPayload::AutoRetried(PlanTaskAutoRetriedEventPayload {
                    plan_id: plan_id.to_string(),
                    task_id: tid.to_string(),
                    delegation_id: delegation_id.to_string(),
                    attempt,
                    error,
                    worker_branch,
                })
            });
            return Ok(event.map(|event| DeferredCompletionPush {
                cont: None,
                event: Some(event),
            }));
        }
        CompletionPersistenceAction::Escalated {
            last_error,
            worker_branch,
        } => {
            // bd-2m2u Phase 2d — option A. Re-materialize the continuation
            // with `PlanTaskEscalated` source so the brain awakens with the
            // right discriminator. The originally-built `cont` was created
            // with `PlanTaskFailed`; reuse that artifact's audit_uri /
            // payload but swap the source.
            let escalated_cont = spur_acp::domain::BrainContinuation {
                source: spur_acp::domain::ContinuationSource::PlanTaskEscalated,
                ..cont
            };
            let event = task_id.map(|tid| {
                PlanTaskNotificationEventPayload::Escalated(PlanTaskEscalatedEventPayload {
                    plan_id: plan_id.to_string(),
                    task_id: tid.to_string(),
                    delegation_id: delegation_id.to_string(),
                    attempt,
                    last_error,
                    worker_branch,
                })
            });
            return Ok(Some(DeferredCompletionPush {
                cont: Some(escalated_cont),
                event,
            }));
        }
        CompletionPersistenceAction::Completed(completion_state) => completion_state,
        CompletionPersistenceAction::AlreadyCompleted => return Ok(None),
    };

    // Cancelled completions persist audit but emit no event/continuation.
    if !matches!(
        completion_state,
        CompletionState::AwaitingReview | CompletionState::Failed
    ) {
        return Ok(None);
    }

    let event = task_id.map(|tid| {
        PlanTaskNotificationEventPayload::Terminal(PlanTaskTerminalEventPayload {
            plan_id: plan_id.to_string(),
            task_id: tid.to_string(),
            delegation_id: delegation_id.to_string(),
            attempt,
            completion_state,
            result_status: result.status.clone(),
        })
    });
    let _ = source; // ContinuationSource lives on `cont` already; field elided.
    Ok(Some(DeferredCompletionPush {
        cont: Some(cont),
        event,
    }))
}

pub(crate) fn completion_state_from_status(
    status: &DelegationStatus,
) -> crate::plan::audit_sentinel::CompletionState {
    match status {
        DelegationStatus::Success | DelegationStatus::Modified { .. } => {
            crate::plan::audit_sentinel::CompletionState::AwaitingReview
        }
        DelegationStatus::Failed { .. } => crate::plan::audit_sentinel::CompletionState::Failed,
        DelegationStatus::Cancelled { .. } => {
            crate::plan::audit_sentinel::CompletionState::Cancelled
        }
        _ => crate::plan::audit_sentinel::CompletionState::Failed,
    }
}

fn failure_reason_from_status(status: &DelegationStatus) -> Option<String> {
    match status {
        DelegationStatus::Failed { error } => Some(error.clone()),
        DelegationStatus::SetupFailed { error } => Some(error.to_string()),
        DelegationStatus::Cancelled { reason } => Some(reason.clone()),
        DelegationStatus::Success | DelegationStatus::Modified { .. } => None,
        other => Some(format!("{other:?}")),
    }
}

/// Payload `persist_completion_inner` packages for the caller to emit AFTER
/// updating the in-memory plan state. Held by [`DeferredCompletionPush`] and
/// consumed by [`DeferredCompletionPush::deliver`].
pub struct PlanTaskTerminalEventPayload {
    pub plan_id: String,
    pub task_id: String,
    pub delegation_id: String,
    pub attempt: u32,
    pub completion_state: crate::plan::audit_sentinel::CompletionState,
    pub result_status: DelegationStatus,
}

pub struct PlanTaskAutoRetriedEventPayload {
    pub plan_id: String,
    pub task_id: String,
    pub delegation_id: String,
    pub attempt: u32,
    pub error: String,
    pub worker_branch: Option<String>,
}

/// bd-2m2u Phase 2d — payload for the `PlanTaskEscalated` event emitted when
/// a plan task exhausts auto-retry budget (1 attempt) and is promoted to
/// `EscalatedToBrain`.
pub struct PlanTaskEscalatedEventPayload {
    pub plan_id: String,
    pub task_id: String,
    pub delegation_id: String,
    pub attempt: u32,
    pub last_error: String,
    pub worker_branch: Option<String>,
}

pub enum PlanTaskNotificationEventPayload {
    Terminal(PlanTaskTerminalEventPayload),
    AutoRetried(PlanTaskAutoRetriedEventPayload),
    Escalated(PlanTaskEscalatedEventPayload),
}

/// Deferred plan-task notifications a plan-completion caller MUST drain after
/// updating in-memory `PlanState` and dropping the lock. Carries the optional
/// brain re-prompt continuation and observability event payload so emission
/// ordering is consistent across observers — events and continuations both
/// fire AFTER the in-memory state reflects the completion or retry decision.
///
/// Contract: caller calls `Self::deliver(...)` exactly once, AFTER updating
/// the in-memory plan state. Dropping the value silently elides notifications
/// and is rejected at compile time by `#[must_use]`.
#[must_use = "deferred plan-task notifications dropped"]
pub struct DeferredCompletionPush {
    pub cont: Option<spur_acp::domain::BrainContinuation>,
    pub event: Option<PlanTaskNotificationEventPayload>,
}

impl DeferredCompletionPush {
    /// Emit the plan-task event and, when present, push the brain continuation.
    /// Call AFTER updating the in-memory PlanState and dropping the lock.
    pub async fn deliver(
        self,
        event_sink: Option<&dyn crate::events::McpEventSink>,
        continuation_ctx: &crate::server::DetachedContinuationCtx,
    ) {
        if let Some(payload) = self.event.as_ref() {
            emit_plan_task_notification_event(event_sink, payload);
        }
        if let Some(cont) = self.cont {
            let delegation_id = cont.delegation_id.as_str().to_string();
            (continuation_ctx.on_complete)(cont, delegation_id).await;
        }
    }
}

fn emit_plan_task_notification_event(
    sink: Option<&dyn crate::events::McpEventSink>,
    payload: &PlanTaskNotificationEventPayload,
) {
    match payload {
        PlanTaskNotificationEventPayload::Terminal(payload) => {
            emit_plan_task_terminal_event(sink, payload);
        }
        PlanTaskNotificationEventPayload::AutoRetried(payload) => {
            emit_plan_task_auto_retried_event(sink, payload);
        }
        PlanTaskNotificationEventPayload::Escalated(payload) => {
            emit_plan_task_escalated_event(sink, payload);
        }
    }
}

fn emit_plan_task_terminal_event(
    sink: Option<&dyn crate::events::McpEventSink>,
    payload: &PlanTaskTerminalEventPayload,
) {
    let Some(sink) = sink else {
        return;
    };
    match payload.completion_state {
        crate::plan::audit_sentinel::CompletionState::AwaitingReview => {
            sink.emit(spur_acp::SpurEventBody::PlanTaskAwaitingReview {
                plan_id: payload.plan_id.clone(),
                task_id: payload.task_id.clone(),
                delegation_id: payload.delegation_id.clone(),
            });
        }
        crate::plan::audit_sentinel::CompletionState::Failed => {
            let error = match &payload.result_status {
                DelegationStatus::Failed { error } => error.clone(),
                DelegationStatus::SetupFailed { error } => error.to_string(),
                other => format!("{other:?}"),
            };
            sink.emit(spur_acp::SpurEventBody::PlanTaskFailed {
                plan_id: payload.plan_id.clone(),
                task_id: payload.task_id.clone(),
                attempt: payload.attempt,
                max_attempts: MAX_ATTEMPTS,
                error,
                delegation_id: payload.delegation_id.clone(),
            });
        }
        crate::plan::audit_sentinel::CompletionState::Cancelled
        | crate::plan::audit_sentinel::CompletionState::Superseded => {}
    }
}

fn emit_plan_task_auto_retried_event(
    sink: Option<&dyn crate::events::McpEventSink>,
    payload: &PlanTaskAutoRetriedEventPayload,
) {
    let Some(sink) = sink else {
        return;
    };
    sink.emit(spur_acp::SpurEventBody::PlanTaskAutoRetried {
        plan_id: payload.plan_id.clone(),
        task_id: payload.task_id.clone(),
        delegation_id: payload.delegation_id.clone(),
        attempt: payload.attempt,
        max_attempts: MAX_ATTEMPTS,
        error: payload.error.clone(),
        worker_branch: payload.worker_branch.clone(),
    });
}

fn emit_plan_task_escalated_event(
    sink: Option<&dyn crate::events::McpEventSink>,
    payload: &PlanTaskEscalatedEventPayload,
) {
    let Some(sink) = sink else {
        return;
    };
    sink.emit(spur_acp::SpurEventBody::PlanTaskEscalated {
        plan_id: payload.plan_id.clone(),
        task_id: payload.task_id.clone(),
        delegation_id: payload.delegation_id.clone(),
        attempt: payload.attempt,
        max_attempts: MAX_ATTEMPTS,
        last_error: payload.last_error.clone(),
        worker_branch: payload.worker_branch.clone(),
    });
}

async fn materialize_and_push_detached_continuation(
    continuation_ctx: &crate::server::DetachedContinuationCtx,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
    result: &DelegationResult,
    delegation_id: &str,
    attempt: u32,
    brain_session_id: &BrainSessionId,
    source: spur_acp::domain::ContinuationSource,
) {
    let cont = materializer
        .materialize(
            result.clone(),
            spur_acp::DelegationId::from(delegation_id),
            attempt,
            brain_session_id.clone(),
            source,
            None,
        )
        .await;
    (continuation_ctx.on_complete)(cont, delegation_id.to_string()).await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn push_plan_completed_continuation(
    continuation_ctx: &crate::server::DetachedContinuationCtx,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
    brain_session_id: &BrainSessionId,
    plan_id: &str,
    approved_count: u32,
    rejected_count: u32,
    failed_count: u32,
    cancelled_count: u32,
) {
    let delegation_id = format!("plan::{plan_id}::completed");
    let result = DelegationResult {
        status: DelegationStatus::Success,
        diff: None,
        diff_summary: None,
        summary: Some(format!(
            "Plan completed: {approved_count} approved, {rejected_count} rejected, {failed_count} failed, {cancelled_count} cancelled"
        )),
        estimated_cost_usd: 0.0,
        worker_branch: None,
        artifact: None,
    };
    materialize_and_push_detached_continuation(
        continuation_ctx,
        materializer,
        &result,
        &delegation_id,
        1,
        brain_session_id,
        spur_acp::domain::ContinuationSource::PlanCompleted,
    )
    .await;
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
    let mut n_blocked_on_setup_conflict = 0usize;
    let mut n_escalated = 0usize;

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
            PlanTaskStatus::BlockedOnSetupConflict { .. } => n_blocked_on_setup_conflict += 1,
            // bd-2m2u Phase 2d — surfaced separately so the overall plan
            // status can render "escalated" instead of falling into "running".
            PlanTaskStatus::EscalatedToBrain { .. } => n_escalated += 1,
        }
    }

    let all_workers_done = n_dispatched == 0
        && n_pending == 0
        && n_ready == 0
        && n_blocked_on_setup_conflict == 0
        && n_escalated == 0;
    let ready_to_merge = all_workers_done
        && n_awaiting_review == 0
        && n_rejected == 0
        && n_failed == 0
        && n_cancelled == 0
        && n_approved == total;

    let overall = if n_blocked_on_setup_conflict > 0 {
        "blocked_on_setup_conflict"
    } else if n_escalated > 0 {
        // bd-2m2u Phase 2d — escalation is a non-terminal "wait on brain"
        // state. Surface it ahead of "running" / "awaiting_review" so the
        // overall string communicates the bottleneck unambiguously.
        "escalated"
    } else if n_dispatched > 0 || n_pending > 0 || n_ready > 0 {
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
                PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
                    obj["status"] = "blocked_on_setup_conflict".into();
                    obj["dep_task_id"] = dep_task_id.clone().into();
                    obj["files"] = serde_json::json!(files);
                }
                PlanTaskStatus::EscalatedToBrain { last_error } => {
                    obj["status"] = "escalated_to_brain".into();
                    obj["last_error"] = last_error.clone().into();
                    if let Some(ref wb) = t.worker_branch {
                        obj["worker_branch"] = wb.clone().into();
                    }
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
        "blocked_on_setup_conflict" => {
            "Resolve the setup overlay conflict, then retry the blocked task.".to_string()
        }
        "escalated" => {
            "One or more tasks exhausted auto-retry budget (1 attempt). Inspect the failed worker branch and call the submit_plan_mutation tool (your 'Swiss Army knife') to resolve by retrying, modifying the spec, or abandoning the task.".to_string()
        }
        "has_failures" => "Some tasks failed. Use get_task_diff to inspect failures.".to_string(),
        "has_rejections" => "Some tasks rejected. Revise the plan or re-submit.".to_string(),
        "failed" => "All tasks failed. Use get_task_diff to inspect errors.".to_string(),
        _ => String::new(),
    };

    serde_json::json!({
        "plan_id": plan_id,
        "status": overall,
        "progress": format!(
            "{reviewed}/{total} reviewed, {n_dispatched} running, {n_pending} pending, {n_blocked_on_setup_conflict} blocked, {n_failed} failed"
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
            "escalated": n_escalated,
            "blocked_on_setup_conflict": n_blocked_on_setup_conflict,
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
///
/// bd-2m2u Phase 2d — `"escalated"` is NOT terminal: brain
/// `submit_plan_mutation` resumes traversal.
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
/// Optionally syncs with beads (pm) and emits events (sink).
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
    reuse_prior_worktree: bool,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
    sink: Option<&dyn crate::events::McpEventSink>,
    _delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    _task_tracker: Option<&tokio_util::task::TaskTracker>,
    _plan_arc: Option<std::sync::Arc<tokio::sync::Mutex<PlanState>>>,
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
                    PlanTaskStatus::BlockedOnSetupConflict { .. } => "blocked_on_setup_conflict",
                    PlanTaskStatus::EscalatedToBrain { .. } => "escalated_to_brain",
                    _ => "unknown",
                };
                return Err(format!(
                    "task '{task_id}' is not awaiting review (current status: {name})"
                ));
            }
        }
    };

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
                    let update = approve_review_update(pm.closed_status(), comment);
                    if let Err(e) = apply_issue_update(pm, id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }

            crate::plan::projector::recompute_open_statuses(&mut state.tasks);
            warnings
                .push("approval persisted; reconciler will pick up newly-ready tasks".to_string());
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
                    let update = reject_review_update(pm.closed_status(), comment);
                    if let Err(e) = apply_issue_update(pm, id, update).await {
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
                            let update = reject_review_update(pm.closed_status(), comment);
                            if let Err(e) = apply_issue_update(pm, id, update).await {
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

            // Re-bind entry mutably for the normal path (the MAX_ATTEMPTS
            // check above used a scoped borrow that has been dropped).
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            let superseded_branch = entry.worker_branch.clone();
            if reuse_prior_worktree && superseded_branch.is_none() {
                return Err(
                    "reuse_prior_worktree=true requires a worker_branch on the rejected attempt"
                        .to_string(),
                );
            }

            let current_record = AttemptRecord {
                attempt: entry.attempt,
                worker_branch: superseded_branch.clone(),
                diff_summary: entry.result.as_ref().and_then(|r| r.diff_summary.clone()),
                summary: entry
                    .result
                    .as_ref()
                    .and_then(|r| r.summary.clone())
                    .or_else(|| summary.clone()),
                feedback: fb.to_string(),
                // Preserve the rejected attempt's dispatched_base_oid for forensics.
                // `.take()` moves the value into history and clears entry so the next
                // attempt starts fresh; T9 will populate it on re-dispatch.
                dispatched_base_oid: entry.dispatched_base_oid.take(),
                reuse_prior_worktree: reuse_prior_worktree.then_some(true),
            };
            entry.history.push(current_record);
            entry.result = None;
            entry.worker_branch = None;
            entry.status = PlanTaskStatus::Pending;

            let issue_id_for_audit = entry.spec.issue_id.clone();
            // Capture before result is reset by the lines above (it's already
            // None here, but the summary fallback is computed against `summary`).
            let attempt_summary = entry
                .result
                .as_ref()
                .and_then(|r| r.summary.clone())
                .or_else(|| summary.clone());
            let attempt_no = entry.attempt;
            let attempt_delegation_id = entry.last_delegation_id.clone().unwrap_or_default();
            if let (Some(pm), Some(id)) = (pm, issue_id_for_audit.as_ref()) {
                let comment = format_request_changes_comment(
                    fb,
                    attempt_no,
                    MAX_ATTEMPTS,
                    superseded_branch.as_deref(),
                );
                let sentinel = audit_sentinel::encode_comment(
                    &audit_sentinel::AuditSentinelKind::ReviewFeedback {
                        delegation_id: attempt_delegation_id.clone(),
                        attempt: attempt_no,
                        feedback: fb.to_string(),
                        worker_branch: superseded_branch.clone(),
                        summary: attempt_summary.clone(),
                        reuse_prior_worktree: reuse_prior_worktree.then_some(true),
                    },
                );
                let update = spur_pm::IssueUpdate {
                    status: Some("open".to_string()),
                    remove_labels: review_ready_label_removals(),
                    comment: Some(comment),
                    ..Default::default()
                };
                if let Err(e) = apply_issue_update(pm, id, update).await {
                    warnings.push(format!("beads comment failed: {e}"));
                }
                let sentinel_update = spur_pm::IssueUpdate {
                    comment: Some(sentinel),
                    ..Default::default()
                };
                if let Err(e) = apply_issue_update(pm, id, sentinel_update).await {
                    warnings.push(format!("audit sentinel comment failed: {e}"));
                }
            }
            warnings.push(
                "request_changes persisted; reconciler will redispatch when ready".to_string(),
            );
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
    }

    Ok(resp)
}

// ─── INV-5: plan-lock-free review ────────────────────────────────────────────

/// Trait abstraction over `PmService` so tests can inject a sleeping mock
/// without standing up a real beads/GitHub backend.
#[async_trait::async_trait]
pub trait PmLike: Send + Sync + 'static {
    async fn get_issue(&self, _id: &str) -> anyhow::Result<spur_pm::Issue> {
        anyhow::bail!("PmLike::get_issue is not implemented for this test fake")
    }
    async fn list_issues(
        &self,
        _filter: spur_pm::IssueFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        anyhow::bail!("PmLike::list_issues is not implemented for this test fake")
    }
    async fn create_issue(&self, _params: spur_pm::IssueCreate) -> anyhow::Result<String> {
        anyhow::bail!("PmLike::create_issue is not implemented for this test fake")
    }
    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()>;
    async fn add_dependency(&self, _issue_id: &str, _depends_on_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("PmLike::add_dependency is not implemented for this test fake")
    }
    async fn create_pr(&self, _params: spur_pm::PrParams) -> anyhow::Result<String> {
        anyhow::bail!("PmLike::create_pr is not implemented for this test fake")
    }
    async fn poll(&self) -> anyhow::Result<Vec<spur_pm::PmEvent>> {
        Ok(Vec::new())
    }
    async fn issue_labels(&self, _id: &str) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn closed_status(&self) -> &str;
    fn source_str(&self) -> &'static str {
        "mock"
    }
    fn issue_graph_available(&self) -> bool {
        false
    }
    async fn issue_subgraph_json(
        &self,
        _id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph> {
        anyhow::bail!("issue graph unavailable for this backend")
    }
    /// Returns the `BeadsAdvanced` extension surface if the backend is beads.
    /// Returns `None` for non-beads backends (GitHub) and test fakes.
    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        None
    }
}

#[async_trait::async_trait]
impl PmLike for spur_pm::PmService {
    async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
        spur_pm::PmService::get_issue(self, id).await
    }
    async fn list_issues(
        &self,
        filter: spur_pm::IssueFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        spur_pm::PmService::list_issues(self, filter).await
    }
    async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
        spur_pm::PmService::create_issue(self, params).await
    }
    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        spur_pm::PmService::update_issue(self, id, update).await
    }
    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        spur_pm::PmService::add_dependency(self, issue_id, depends_on_id).await
    }
    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        spur_pm::PmService::create_pr(self, params).await
    }
    async fn poll(&self) -> anyhow::Result<Vec<spur_pm::PmEvent>> {
        spur_pm::PmService::poll(self).await
    }
    async fn issue_labels(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(spur_pm::PmService::get_issue(self, id).await?.labels)
    }
    fn closed_status(&self) -> &str {
        spur_pm::PmService::closed_status(self)
    }
    fn source_str(&self) -> &'static str {
        spur_pm::PmService::source_str(self)
    }
    fn issue_graph_available(&self) -> bool {
        spur_pm::PmService::issue_graph_available(self)
    }
    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph> {
        spur_pm::PmService::issue_subgraph_json(self, id).await
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewWriteMode {
    Advisory,
    NonAdvisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReviewBeadsVersion(u64);

const REVIEW_WRITE_BACKOFFS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2_000),
];

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
}

/// Pending audit sentinel emission: collected by `apply_decision_and_extract`
/// (sync, under the plan lock) and flushed by `handle_review_task` after the
/// lock is released. Advisory — failures are logged at WARN and continue.
enum PendingAuditEmit {
    Approval {
        issue_id: Option<String>,
        plan_id: String,
        task_id: String,
        delegation_id: String,
    },
    Rejection {
        issue_id: Option<String>,
        plan_id: String,
        task_id: String,
        delegation_id: String,
        feedback: String,
    },
    ReviewFeedback {
        issue_id: Option<String>,
        plan_id: String,
        task_id: String,
        delegation_id: String,
        attempt: u32,
        feedback: String,
        worker_branch: Option<String>,
        summary: Option<String>,
        reuse_prior_worktree: Option<bool>,
    },
}

impl PendingAuditEmit {
    fn into_beads_ops(self, epic_id: Option<&str>) -> Vec<PendingBeadsOp> {
        let mut ops = Vec::with_capacity(2);
        match self {
            PendingAuditEmit::Approval {
                issue_id,
                plan_id,
                task_id,
                delegation_id,
            } => {
                if let Some(issue_id) = issue_id {
                    let kind = audit_sentinel::AuditSentinelKind::Approval { delegation_id };
                    ops.push(PendingBeadsOp {
                        issue_id,
                        update: spur_pm::IssueUpdate {
                            comment: Some(audit_sentinel::encode_comment(&kind)),
                            ..Default::default()
                        },
                    });
                }
                push_task_transition_audit(
                    &mut ops,
                    epic_id,
                    plan_id,
                    task_id,
                    "awaiting_review",
                    "approved",
                );
            }
            PendingAuditEmit::Rejection {
                issue_id,
                plan_id,
                task_id,
                delegation_id,
                feedback,
            } => {
                if let Some(issue_id) = issue_id {
                    let kind = audit_sentinel::AuditSentinelKind::Rejection {
                        delegation_id,
                        feedback,
                    };
                    ops.push(PendingBeadsOp {
                        issue_id,
                        update: spur_pm::IssueUpdate {
                            comment: Some(audit_sentinel::encode_comment(&kind)),
                            ..Default::default()
                        },
                    });
                }
                push_task_transition_audit(
                    &mut ops,
                    epic_id,
                    plan_id,
                    task_id,
                    "awaiting_review",
                    "rejected",
                );
            }
            PendingAuditEmit::ReviewFeedback {
                issue_id,
                plan_id,
                task_id,
                delegation_id,
                attempt,
                feedback,
                worker_branch,
                summary,
                reuse_prior_worktree,
            } => {
                if let Some(issue_id) = issue_id {
                    let kind = audit_sentinel::AuditSentinelKind::ReviewFeedback {
                        delegation_id,
                        attempt,
                        feedback,
                        worker_branch,
                        summary,
                        reuse_prior_worktree,
                    };
                    ops.push(PendingBeadsOp {
                        issue_id,
                        update: spur_pm::IssueUpdate {
                            comment: Some(audit_sentinel::encode_comment(&kind)),
                            ..Default::default()
                        },
                    });
                }
                push_task_transition_audit(
                    &mut ops,
                    epic_id,
                    plan_id,
                    task_id,
                    "awaiting_review",
                    "pending",
                );
            }
        }
        ops
    }
}

fn push_task_transition_audit(
    ops: &mut Vec<PendingBeadsOp>,
    epic_id: Option<&str>,
    plan_id: String,
    task_id: String,
    from_status: &str,
    to_status: &str,
) {
    let Some(epic_id) = epic_id else {
        return;
    };
    let kind = audit_sentinel::AuditSentinelKind::TaskTransition {
        plan_id,
        task_id,
        from_status: from_status.to_string(),
        to_status: to_status.to_string(),
    };
    ops.push(PendingBeadsOp {
        issue_id: epic_id.to_string(),
        update: spur_pm::IssueUpdate {
            comment: Some(audit_sentinel::encode_comment(&kind)),
            ..Default::default()
        },
    });
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
    reuse_prior_worktree: bool,
    state: &mut PlanState,
    pm_closed_status: Option<&str>,
    _delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    _task_tracker: Option<&tokio_util::task::TaskTracker>,
    _plan_arc: Option<std::sync::Arc<tokio::sync::Mutex<PlanState>>>,
    _sink: Option<&dyn crate::events::McpEventSink>,
    _pm_arc: Option<&Arc<dyn PmLike>>,
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
                    PlanTaskStatus::BlockedOnSetupConflict { .. } => "blocked_on_setup_conflict",
                    PlanTaskStatus::EscalatedToBrain { .. } => "escalated_to_brain",
                    _ => "unknown",
                };
                return Err(format!(
                    "task '{task_id}' is not awaiting review (current status: {name})"
                ));
            }
        }
    };

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
                task_id: task_id.to_string(),
                delegation_id: last_del_id.unwrap_or_default(),
            });

            // Stage beads sync — executes outside the lock.
            if let (Some(closed_status), Some(id)) = (pm_closed_status, issue_id) {
                let comment = format!(
                    "Brain approved: {}",
                    feedback.unwrap_or("meets acceptance criteria")
                );
                let update = approve_review_update(closed_status, comment);
                beads_ops.push(PendingBeadsOp {
                    issue_id: id,
                    update,
                });
            }

            crate::plan::projector::recompute_open_statuses(&mut state.tasks);
            warnings
                .push("approval persisted; reconciler will pick up newly-ready tasks".to_string());
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
                task_id: task_id.to_string(),
                delegation_id: last_del_id.unwrap_or_default(),
                feedback: feedback_str.to_string(),
            });

            // Stage beads sync — executes outside the lock.
            if let Some(id) = issue_id {
                let comment = format!("Brain rejected: {feedback_str}");
                let update = reject_review_update(pm_closed_status.unwrap_or("closed"), comment);
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
                        task_id: task_id.to_string(),
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
                        let update =
                            reject_review_update(pm_closed_status.unwrap_or("closed"), comment);
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

            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            let superseded_branch = entry.worker_branch.clone();
            if reuse_prior_worktree && superseded_branch.is_none() {
                return Err(
                    "reuse_prior_worktree=true requires a worker_branch on the rejected attempt"
                        .to_string(),
                );
            }

            let current_record = AttemptRecord {
                attempt: entry.attempt,
                worker_branch: superseded_branch.clone(),
                diff_summary: entry.result.as_ref().and_then(|r| r.diff_summary.clone()),
                summary: entry
                    .result
                    .as_ref()
                    .and_then(|r| r.summary.clone())
                    .or_else(|| summary.clone()),
                feedback: fb.to_string(),
                // Preserve the rejected attempt's dispatched_base_oid for forensics.
                // `.take()` moves the value into history and clears entry so the next
                // attempt starts fresh; T9 will populate it on re-dispatch.
                dispatched_base_oid: entry.dispatched_base_oid.take(),
                reuse_prior_worktree: reuse_prior_worktree.then_some(true),
            };
            entry.history.push(current_record);
            entry.result = None;
            entry.worker_branch = None;
            entry.status = PlanTaskStatus::Pending;

            let issue_id_for_audit = entry.spec.issue_id.clone();
            let attempt_summary = entry
                .result
                .as_ref()
                .and_then(|r| r.summary.clone())
                .or_else(|| summary.clone());
            let attempt_no = entry.attempt;
            let attempt_delegation_id = entry.last_delegation_id.clone().unwrap_or_default();
            if let Some(id) = issue_id_for_audit {
                let comment = format_request_changes_comment(
                    fb,
                    attempt_no,
                    MAX_ATTEMPTS,
                    superseded_branch.as_deref(),
                );
                let update = spur_pm::IssueUpdate {
                    status: Some("open".to_string()),
                    remove_labels: review_ready_label_removals(),
                    comment: Some(comment),
                    ..Default::default()
                };
                beads_ops.push(PendingBeadsOp {
                    issue_id: id.clone(),
                    update,
                });
                audit_emits.push(PendingAuditEmit::ReviewFeedback {
                    issue_id: Some(id),
                    plan_id: plan_id.to_string(),
                    task_id: task_id.to_string(),
                    delegation_id: attempt_delegation_id,
                    attempt: attempt_no,
                    feedback: fb.to_string(),
                    worker_branch: superseded_branch.clone(),
                    summary: attempt_summary,
                    reuse_prior_worktree: reuse_prior_worktree.then_some(true),
                });
            }
            warnings.push(
                "request_changes persisted; reconciler will redispatch when ready".to_string(),
            );
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
    }

    let events = vec![PendingEvent::TaskReviewed {
        plan_id: plan_id.to_string(),
        task_id: task_id.to_string(),
        task_name: Some(task_name.clone()),
        decision: decision.to_string(),
        feedback: feedback.map(String::from),
        attempt: current_attempt,
    }];

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
    reuse_prior_worktree: bool,
    pm: Option<Arc<dyn PmLike>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
    feature_gate: Arc<spur_license::FeatureGate>,
) -> Result<serde_json::Value, String> {
    handle_review_task_with_write_mode(
        plan_arc,
        plan_id,
        task_id,
        decision,
        feedback,
        reuse_prior_worktree,
        pm,
        sink,
        delegation_tx,
        task_tracker,
        feature_gate,
        ReviewWriteMode::Advisory,
    )
    .await
}

async fn review_beads_version(
    pm: &dyn PmLike,
    feature_gate: &spur_license::FeatureGate,
    issue_ids: &BTreeSet<String>,
) -> Result<ReviewBeadsVersion, String> {
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(crate::server::feature_error_message)?;
    let Some(advanced) = pm.advanced() else {
        return Err("non-advisory review writes require beads advanced read-back".to_string());
    };
    let mut total = 0u64;
    for issue_id in issue_ids {
        let comments = advanced.list_comments(issue_id).await.map_err(|error| {
            format!("read-back comments for issue '{issue_id}' failed: {error}")
        })?;
        total += comments.len() as u64;
    }
    Ok(ReviewBeadsVersion(total))
}

async fn apply_review_ops_nonadvisory(
    pm: &dyn PmLike,
    feature_gate: &spur_license::FeatureGate,
    ops: Vec<PendingBeadsOp>,
) -> Result<(), String> {
    if ops.is_empty() {
        return Ok(());
    }

    let issue_ids: BTreeSet<String> = ops.iter().map(|op| op.issue_id.clone()).collect();
    let before = review_beads_version(pm, feature_gate, &issue_ids).await?;
    let mut last_error = String::new();
    let mut succeeded = vec![false; ops.len()];

    for (attempt_idx, backoff) in REVIEW_WRITE_BACKOFFS.iter().enumerate() {
        let attempt_no = attempt_idx + 1;
        let mut write_failed = false;
        for (op_idx, op) in ops.iter().enumerate() {
            if succeeded[op_idx] {
                continue;
            }
            if let Err(error) = apply_issue_update(pm, &op.issue_id, op.update.clone()).await {
                write_failed = true;
                last_error = format!(
                    "non-advisory review write attempt {attempt_no}/{} failed for issue '{}': {error}",
                    REVIEW_WRITE_BACKOFFS.len(),
                    op.issue_id
                );
                warn!("{last_error}");
                break;
            }
            // update_issue(comment=...) appends to beads. Track per-op success
            // so retrying a later failure does not duplicate audit comments.
            succeeded[op_idx] = true;
        }

        if !write_failed {
            // INV-S1: only the caller may install `candidate_state` in the
            // cache, and only after this read-back proves the substrate
            // version advanced. INV-S4: the audit ops above are in the same
            // bounded write batch as the task status/label mutation.
            match review_beads_version(pm, feature_gate, &issue_ids).await {
                Ok(after) if after > before => return Ok(()),
                Ok(after) => {
                    last_error = format!(
                        "non-advisory review write attempt {attempt_no}/{} did not advance BeadsVersion (before: {before:?}, after: {after:?})",
                        REVIEW_WRITE_BACKOFFS.len()
                    );
                }
                Err(error) => {
                    last_error = format!(
                        "non-advisory review write attempt {attempt_no}/{} read-back failed: {error}",
                        REVIEW_WRITE_BACKOFFS.len()
                    );
                }
            }
            warn!("{last_error}");
        }

        if attempt_no < REVIEW_WRITE_BACKOFFS.len() {
            tokio::time::sleep(*backoff).await;
        }
    }

    Err(last_error)
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_review_task_with_write_mode(
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    reuse_prior_worktree: bool,
    pm: Option<Arc<dyn PmLike>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
    feature_gate: Arc<spur_license::FeatureGate>,
    write_mode: ReviewWriteMode,
) -> Result<serde_json::Value, String> {
    let pm_closed_status = pm.as_deref().map(|p| p.closed_status().to_string());

    // 1) Sync mutation under lock — no .await inside this block.
    let (mut outcome, candidate_state) = {
        let state = plan_arc.lock().await;
        let mut candidate = state.clone();
        apply_decision_and_extract(
            plan_id,
            task_id,
            decision,
            feedback,
            reuse_prior_worktree,
            &mut candidate,
            pm_closed_status.as_deref(),
            delegation_tx,
            task_tracker,
            Some(plan_arc.clone()),
            sink,
            pm.as_ref(),
        )
        .map(|outcome| (outcome, candidate))
    }?; // lock released here.

    // 2) Async beads I/O — outside the lock.
    if let Some(pm) = pm.as_deref() {
        match write_mode {
            ReviewWriteMode::Advisory => {
                for op in std::mem::take(&mut outcome.beads_ops) {
                    if let Err(e) = apply_issue_update(pm, &op.issue_id, op.update).await {
                        // Beads failures are best-effort; already baked into warnings
                        // inside the response if any were anticipated.
                        warn!(
                            "handle_review_task: beads update failed for {}: {e}",
                            op.issue_id
                        );
                    }
                }
            }
            ReviewWriteMode::NonAdvisory => {
                let mut ops = std::mem::take(&mut outcome.beads_ops);
                let epic_id = candidate_state.epic_id.as_deref();
                ops.extend(
                    std::mem::take(&mut outcome.audit_emits)
                        .into_iter()
                        .flat_map(|emit| emit.into_beads_ops(epic_id)),
                );
                apply_review_ops_nonadvisory(pm, feature_gate.as_ref(), ops).await?;
            }
        }

        // 2b) Flush audit sentinel emissions — advisory, outside the lock.
        if matches!(write_mode, ReviewWriteMode::Advisory) {
            for emit in std::mem::take(&mut outcome.audit_emits) {
                match emit {
                    PendingAuditEmit::Approval {
                        issue_id,
                        plan_id,
                        task_id: _,
                        delegation_id,
                    } => {
                        emit_approval_audit(
                            Some(pm),
                            &issue_id,
                            feature_gate.as_ref(),
                            &plan_id,
                            &delegation_id,
                        )
                        .await;
                    }
                    PendingAuditEmit::Rejection {
                        issue_id,
                        plan_id,
                        task_id: _,
                        delegation_id,
                        feedback,
                    } => {
                        emit_rejection_audit(
                            Some(pm),
                            &issue_id,
                            feature_gate.as_ref(),
                            &plan_id,
                            &delegation_id,
                            &feedback,
                        )
                        .await;
                    }
                    PendingAuditEmit::ReviewFeedback {
                        issue_id,
                        plan_id,
                        task_id: _,
                        delegation_id,
                        attempt,
                        feedback,
                        worker_branch,
                        summary,
                        reuse_prior_worktree,
                    } => {
                        emit_review_feedback_audit(
                            Some(pm),
                            &issue_id,
                            feature_gate.as_ref(),
                            &plan_id,
                            &delegation_id,
                            attempt,
                            &feedback,
                            worker_branch,
                            summary,
                            reuse_prior_worktree,
                        )
                        .await;
                    }
                }
            }
        }
    }

    // INV-S1: update the cache only after substrate persistence has completed.
    // In non-advisory mode, audit sentinels are included in the write set
    // (INV-S4) and read-back must advance before this assignment.
    {
        let mut state = plan_arc.lock().await;
        *state = candidate_state;
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

// ─── Test support ────────────────────────────────────────────────────

/// Utilities for integration tests that need crate-internal plan hooks.
#[doc(hidden)]
pub mod test_support {
    use super::PmLike;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    pub const ORPHAN_CLEAR_REASON_RESTART: &str = crate::server::ORPHAN_CLEAR_REASON_RESTART;

    #[allow(clippy::too_many_arguments)]
    pub async fn persist_worker_completion_and_notify(
        pm: &dyn PmLike,
        issue_id: &str,
        feature_gate: &spur_license::FeatureGate,
        plan_id: &str,
        delegation_id: &str,
        fast_forward: &Option<Arc<tokio::sync::Notify>>,
        result: &spur_acp::DelegationResult,
        brain_session_id: &spur_acp::BrainSessionId,
        attempt: u32,
        materializer: &crate::outcome_materializer::OutcomeMaterializer,
        dispatched_base_oid: Option<String>,
    ) -> anyhow::Result<Option<super::DeferredCompletionPush>> {
        super::persist_worker_completion_and_notify(
            pm,
            issue_id,
            feature_gate,
            plan_id,
            delegation_id,
            fast_forward,
            result,
            brain_session_id,
            attempt,
            materializer,
            dispatched_base_oid,
            None,
        )
        .await
    }

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
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn task(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.into(),
            agent: "test-agent".into(),
            task: "test task".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            issue_title: None,
            context_files: vec![],
        }
    }

    fn test_materializer() -> Arc<crate::outcome_materializer::OutcomeMaterializer> {
        Arc::new(crate::outcome_materializer::OutcomeMaterializer::new(
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        ))
    }

    fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
        let gate = Arc::new(spur_license::FeatureGate::new(
            spur_license::policy::PolicyResolver::embedded(),
        ));
        let features =
            std::collections::BTreeSet::from([spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED
                .as_str()
                .to_string()]);
        gate.update_state(&spur_license::LicenseState::active_validated(
            spur_license::Plan::Pro,
            features,
        ));
        gate
    }

    fn unlicensed_feature_gate() -> Arc<spur_license::FeatureGate> {
        let gate = community_feature_gate();
        let mut snapshot = (**gate.snapshot()).clone();
        snapshot
            .features
            .remove(&spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED);
        gate.set_snapshot_for_test(snapshot);
        gate
    }

    fn community_feature_gate() -> Arc<spur_license::FeatureGate> {
        Arc::new(spur_license::FeatureGate::new(
            spur_license::policy::PolicyResolver::embedded(),
        ))
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
        assert!(!super::is_terminal_plan_status("escalated"));
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
    fn plan_task_entry_serializes_dispatched_base_oid() {
        let entry = super::PlanTaskEntry {
            spec: super::PlanTask {
                task_id: "T1".into(),
                agent: "x".into(),
                task: "do".into(),
                depends_on: vec![],
                issue_id: None,
                issue_title: None,
                context_files: vec![],
            },
            status: super::PlanTaskStatus::Approved { summary: None },
            result: None,
            worker_branch: Some("spur/worker-x-1".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: Some("abc123".into()),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["dispatched_base_oid"], "abc123");
    }

    #[test]
    fn legacy_plan_task_entry_without_dispatched_base_oid_deserializes() {
        let json = serde_json::json!({
            "spec": { "task_id": "T1", "agent": "x", "task": "do", "depends_on": [], "issue_id": null, "context_files": [] },
            "status": { "status": "approved", "summary": null },
            "result": null,
            "worker_branch": null,
            "attempt": 1,
            "history": [],
            "last_delegation_id": null,
        });
        let entry: super::PlanTaskEntry = serde_json::from_value(json).unwrap();
        assert!(entry.dispatched_base_oid.is_none());
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

    fn task_with_files(id: &str, deps: &[&str], files: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.into(),
            agent: "test-agent".into(),
            task: "test task".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            issue_title: None,
            context_files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn find_sibling_overlaps_detects_unrelated_pair_sharing_file() {
        // Two tasks at the same DAG level, neither depends on the other,
        // both touch orchestrator.rs. Expect one overlap entry.
        let tasks = vec![
            task_with_files("A", &[], &["crates/spur-core/src/orchestrator.rs"]),
            task_with_files("B", &[], &["crates/spur-core/src/orchestrator.rs"]),
        ];
        let overlaps = super::find_sibling_overlaps(&tasks);
        assert_eq!(overlaps.len(), 1);
        let entry = &overlaps[0];
        // The injected edge is deterministic: lexicographically lower id
        // becomes the dep of the higher id (so synthetic edges form an order
        // independent of input task order).
        assert_eq!(entry.from, "A");
        assert_eq!(entry.to, "B");
        assert_eq!(
            entry.shared_files,
            vec!["crates/spur-core/src/orchestrator.rs"]
        );
    }

    #[test]
    fn find_sibling_overlaps_skips_pairs_with_existing_transitive_dep() {
        // A → B → C. A and C share a file but A is already a transitive dep of C.
        let tasks = vec![
            task_with_files("A", &[], &["shared.rs"]),
            task_with_files("B", &["A"], &[]),
            task_with_files("C", &["B"], &["shared.rs"]),
        ];
        let overlaps = super::find_sibling_overlaps(&tasks);
        assert!(
            overlaps.is_empty(),
            "expected no overlaps, got {:?}",
            overlaps
        );
    }

    #[test]
    fn find_sibling_overlaps_skips_disjoint_files() {
        let tasks = vec![
            task_with_files("A", &[], &["foo.rs"]),
            task_with_files("B", &[], &["bar.rs"]),
        ];
        assert!(super::find_sibling_overlaps(&tasks).is_empty());
    }

    #[test]
    fn find_sibling_overlaps_handles_empty_context_files() {
        // A task with no context_files cannot overlap with anything.
        let tasks = vec![
            task_with_files("A", &[], &[]),
            task_with_files("B", &[], &["foo.rs"]),
        ];
        assert!(super::find_sibling_overlaps(&tasks).is_empty());
    }

    #[test]
    fn find_sibling_overlaps_diamond_dag_three_siblings() {
        // The br-77i incident pattern: A depends on root, B/C/D are parallel
        // siblings all touching orchestrator.rs.
        let tasks = vec![
            task_with_files("root", &[], &["root.rs"]),
            task_with_files("B", &["root"], &["orch.rs"]),
            task_with_files("C", &["root"], &["orch.rs"]),
            task_with_files("D", &["root"], &["orch.rs"]),
            task_with_files("sink", &["B", "C", "D"], &[]),
        ];
        let overlaps = super::find_sibling_overlaps(&tasks);
        // Three unordered pairs (B,C), (B,D), (C,D) → three synthetic edges.
        assert_eq!(overlaps.len(), 3);
        let pairs: std::collections::HashSet<(&str, &str)> = overlaps
            .iter()
            .map(|o| (o.from.as_str(), o.to.as_str()))
            .collect();
        assert!(pairs.contains(&("B", "C")));
        assert!(pairs.contains(&("B", "D")));
        assert!(pairs.contains(&("C", "D")));
    }

    #[test]
    fn apply_sibling_overlaps_injects_edges_and_preserves_originals() {
        let mut tasks = vec![
            task_with_files("A", &[], &["shared.rs"]),
            task_with_files("B", &["A"], &["other.rs"]),
            task_with_files("C", &[], &["shared.rs"]),
        ];
        let overlaps = super::find_sibling_overlaps(&tasks);
        assert_eq!(overlaps.len(), 1);
        super::apply_sibling_overlaps(&mut tasks, &overlaps);

        let c = tasks.iter().find(|t| t.task_id == "C").unwrap();
        assert!(
            c.depends_on.iter().any(|d| d == "A"),
            "expected synthetic edge A→C, got {:?}",
            c.depends_on
        );
        let b = tasks.iter().find(|t| t.task_id == "B").unwrap();
        assert_eq!(
            b.depends_on,
            vec!["A".to_string()],
            "B's original deps must be preserved"
        );
    }

    #[test]
    fn apply_sibling_overlaps_idempotent_on_existing_edge() {
        // If somehow the synthetic edge is already present, we must not duplicate.
        let mut tasks = vec![
            task_with_files("A", &[], &["shared.rs"]),
            task_with_files("B", &["A"], &["shared.rs"]),
        ];
        // After find_sibling_overlaps: A and B are related (B depends on A), so
        // no overlap is emitted. Direct invocation simulates a stale synthetic
        // edge slipping through.
        let synthetic = vec![super::SiblingOverlap {
            from: "A".into(),
            to: "B".into(),
            shared_files: vec!["shared.rs".into()],
        }];
        super::apply_sibling_overlaps(&mut tasks, &synthetic);
        let b = tasks.iter().find(|t| t.task_id == "B").unwrap();
        assert_eq!(b.depends_on, vec!["A".to_string()]);
    }

    #[test]
    fn apply_sibling_overlaps_keeps_validate_plan_passing() {
        // After injecting synthetic edges, the resulting plan must still pass
        // validate_plan (no cycles introduced — synthetic edges go lex-lower→higher,
        // and original DAG was acyclic).
        let mut tasks = vec![
            task_with_files("A", &[], &["x.rs"]),
            task_with_files("B", &[], &["x.rs"]),
            task_with_files("C", &[], &["x.rs"]),
        ];
        let overlaps = super::find_sibling_overlaps(&tasks);
        super::apply_sibling_overlaps(&mut tasks, &overlaps);
        super::validate_plan(&tasks).expect("post-injection plan must validate");
    }

    #[test]
    fn submit_plan_normalize_tasks_returns_injected_overlaps() {
        // Diamond-DAG sibling-file-overlap (the br-77i incident pattern).
        let mut tasks = vec![
            task_with_files("root", &[], &["root.rs"]),
            task_with_files("X", &["root"], &["orch.rs"]),
            task_with_files("Y", &["root"], &["orch.rs"]),
            task_with_files("sink", &["X", "Y"], &[]),
        ];
        let overlaps = super::submit_plan_normalize_tasks(&mut tasks)
            .expect("normalize should succeed for valid input");
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].from, "X");
        assert_eq!(overlaps[0].to, "Y");
        let y = tasks.iter().find(|t| t.task_id == "Y").unwrap();
        assert!(
            y.depends_on.iter().any(|d| d == "X"),
            "Y must now depend on X"
        );
    }

    #[test]
    fn submit_plan_normalize_tasks_propagates_validate_errors() {
        let mut tasks = vec![task_with_files("A", &["A"], &[])]; // self-cycle
        let err = super::submit_plan_normalize_tasks(&mut tasks).unwrap_err();
        assert!(err.contains("Cycle"));
    }

    #[test]
    fn br_77i_diamond_dag_orchestrator_rs_serializes_three_siblings() {
        // Reproduces the bd-14cq Wave-1+Wave-2 DAG that triggered br-77i:
        //   orch-server-field (root) →
        //     orch-shutdown-retire | orch-flush-on-exit | orch-inject-mcp-url →
        //       e2e-integration-test (sink)
        // All three Wave-2 tasks touched orchestrator.rs.
        let mut tasks = vec![
            task_with_files(
                "orch-server-field",
                &[],
                &["crates/spur-core/src/orchestrator.rs"],
            ),
            task_with_files(
                "orch-shutdown-retire",
                &["orch-server-field"],
                &["crates/spur-core/src/orchestrator.rs"],
            ),
            task_with_files(
                "orch-flush-on-exit",
                &["orch-server-field"],
                &["crates/spur-core/src/orchestrator.rs"],
            ),
            task_with_files(
                "orch-inject-mcp-url",
                &["orch-server-field"],
                &["crates/spur-core/src/orchestrator.rs"],
            ),
            task_with_files(
                "e2e-integration-test",
                &[
                    "orch-shutdown-retire",
                    "orch-flush-on-exit",
                    "orch-inject-mcp-url",
                ],
                &[],
            ),
        ];
        let overlaps = super::submit_plan_normalize_tasks(&mut tasks).unwrap();

        // Three pairs among Wave-2 siblings: (flush, inject), (flush, shutdown),
        // (inject, shutdown). orch-server-field overlaps with all three Wave-2
        // tasks but is their declared parent → not flagged.
        assert_eq!(overlaps.len(), 3, "got {:?}", overlaps);

        // Verify Wave-2 tasks are now linearly ordered: lex-min depends on
        // nothing extra; lex-mid depends on lex-min; lex-max depends on the
        // other two.
        let flush = tasks
            .iter()
            .find(|t| t.task_id == "orch-flush-on-exit")
            .unwrap();
        let inject = tasks
            .iter()
            .find(|t| t.task_id == "orch-inject-mcp-url")
            .unwrap();
        let shutdown = tasks
            .iter()
            .find(|t| t.task_id == "orch-shutdown-retire")
            .unwrap();
        // Lex order: orch-flush-on-exit < orch-inject-mcp-url < orch-shutdown-retire.
        assert_eq!(flush.depends_on, vec!["orch-server-field"]);
        assert!(inject
            .depends_on
            .contains(&"orch-flush-on-exit".to_string()));
        assert!(shutdown
            .depends_on
            .contains(&"orch-flush-on-exit".to_string()));
        assert!(shutdown
            .depends_on
            .contains(&"orch-inject-mcp-url".to_string()));
    }

    #[test]
    fn enriched_task_includes_original_history_and_feedback() {
        let history = vec![super::AttemptRecord {
            attempt: 1,
            worker_branch: Some("spur/worker-x".to_string()),
            diff_summary: None,
            summary: Some("did thing".to_string()),
            feedback: "add null check".to_string(),
            dispatched_base_oid: None,
            reuse_prior_worktree: None,
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
    fn attempt_record_round_trips_reuse_prior_worktree_true() {
        let record = super::AttemptRecord {
            attempt: 1,
            worker_branch: Some("spur/worker-x".to_string()),
            diff_summary: None,
            summary: Some("did thing".to_string()),
            feedback: "add null check".to_string(),
            dispatched_base_oid: None,
            reuse_prior_worktree: Some(true),
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: super::AttemptRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.reuse_prior_worktree, Some(true));
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
            dispatched_base_oid: None,
            reuse_prior_worktree: None,
        }];
        let enriched = super::build_enriched_task("Task", &history, "more", 2, super::MAX_ATTEMPTS);
        assert!(!enriched.contains("git show"));
    }

    #[test]
    fn build_failure_recovery_task_uses_worker_frame_not_brain_feedback() {
        let history = vec![super::AttemptRecord {
            attempt: 1,
            worker_branch: Some("spur/worker-failed".into()),
            diff_summary: None,
            summary: Some("partial".into()),
            feedback: super::worker_failure_recovery_feedback("worker crashed"),
            dispatched_base_oid: Some("base-oid".into()),
            reuse_prior_worktree: None,
        }];

        let task = super::build_failure_recovery_task(
            "Implement the task",
            &history,
            "worker crashed",
            Some("spur/worker-failed"),
            2,
            super::MAX_ATTEMPTS,
        );

        assert!(task.contains("Implement the task"));
        assert!(task.contains("## Recovery context (Attempt 2 of 3)"));
        assert!(task.contains("Attempt 1: worker crashed"));
        assert!(task.contains("branch: spur/worker-failed"));
        assert!(!task.contains("Brain feedback:"));
    }

    #[test]
    fn auto_retry_amended_prompt_includes_failure_reason_and_branch_state() {
        let history = vec![super::AttemptRecord {
            attempt: 1,
            worker_branch: Some("spur/worker-bd-2m2u".into()),
            diff_summary: None,
            summary: None,
            feedback: super::worker_failure_recovery_feedback("Delegation channel closed"),
            dispatched_base_oid: Some("abc123".into()),
            reuse_prior_worktree: None,
        }];

        let task = super::build_failure_recovery_task(
            "Fix retry handling",
            &history,
            "Delegation channel closed",
            Some("spur/worker-bd-2m2u"),
            2,
            super::MAX_ATTEMPTS,
        );

        assert!(task.contains("Delegation channel closed"));
        assert!(task.contains("spur/worker-bd-2m2u"));
        assert!(task.contains("git log <base>..<branch>"));
        assert!(task.contains("recover from there"));
    }

    #[test]
    fn reconciler_dispatch_picks_failure_recovery_template_for_worker_failure_history() {
        let entry = super::PlanTaskEntry {
            spec: super::PlanTask {
                task_id: "T1".into(),
                agent: "codex".into(),
                task: "Implement worker retry".into(),
                depends_on: vec![],
                issue_id: Some("bd-1".into()),
                issue_title: None,
                context_files: vec![],
            },
            status: super::PlanTaskStatus::Ready,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![super::AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-failed".into()),
                diff_summary: None,
                summary: None,
                feedback: super::worker_failure_recovery_feedback(
                    "worker failed before producing output",
                ),
                dispatched_base_oid: None,
                reuse_prior_worktree: None,
            }],
            last_delegation_id: Some("del-A".into()),
            dispatched_base_oid: None,
        };

        let task_text = super::build_dispatch_task_text(&entry);

        assert!(task_text.contains("## Recovery context"));
        assert!(task_text.contains("worker failed before producing output"));
        assert!(!task_text.contains("Brain feedback:"));
    }

    #[test]
    fn reconciler_dispatch_picks_enriched_template_for_review_feedback_history() {
        let entry = super::PlanTaskEntry {
            spec: super::PlanTask {
                task_id: "T1".into(),
                agent: "codex".into(),
                task: "Implement review feedback".into(),
                depends_on: vec![],
                issue_id: Some("bd-1".into()),
                issue_title: None,
                context_files: vec![],
            },
            status: super::PlanTaskStatus::Ready,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![super::AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-review".into()),
                diff_summary: None,
                summary: Some("partial".into()),
                feedback: "add the missing test".into(),
                dispatched_base_oid: None,
                reuse_prior_worktree: None,
            }],
            last_delegation_id: Some("del-A".into()),
            dispatched_base_oid: None,
        };

        let task_text = super::build_dispatch_task_text(&entry);

        assert!(task_text.contains("Brain feedback: add the missing test"));
        assert!(task_text.contains("## Current Request"));
        assert!(!task_text.contains("## Recovery context"));
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
                issue_title: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "B".to_string(),
                agent: "x".to_string(),
                task: "b".to_string(),
                depends_on: vec!["A".to_string()],
                issue_id: None,
                issue_title: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "C".to_string(),
                agent: "x".to_string(),
                task: "c".to_string(),
                depends_on: vec!["B".to_string()],
                issue_id: None,
                issue_title: None,
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
                    dispatched_base_oid: None,
                })
                .collect(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
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

    #[test]
    fn immediate_send_failure_compensation_removes_dispatch_label() {
        let labels = vec![crate::plan::labels::lease_expires_at(1_777_777_777)];
        let update = super::dispatch_send_failure_update("del-A", &labels);
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::delegation_id("del-A")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::lease_expires_at(1_777_777_777)));
        assert_eq!(
            update.comment.as_deref(),
            Some("Dispatch send failed before worker ownership was established.")
        );
    }

    #[test]
    fn persist_dispatch_intent_update_removes_legacy_labels() {
        let update = super::dispatch_intent_update("del-A", 1_777_777_777, &[]);
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::delegation_id("del-A")));
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::lease_expires_at(1_777_777_777)));
        assert!(update
            .remove_labels
            .contains(&"delegation-id:del-A".to_string()));
        assert!(update
            .remove_labels
            .contains(&"ready-for-review".to_string()));
    }

    #[test]
    fn clear_dispatch_intent_strips_lease_label() {
        let labels = vec![crate::plan::labels::lease_expires_at(1_777_777_777)];
        let update = super::clear_dispatch_intent_update("del-A", &labels);
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::delegation_id("del-A")));
        assert!(update
            .remove_labels
            .contains(&"delegation-id:del-A".to_string()));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::lease_expires_at(1_777_777_777)));
    }

    #[test]
    fn success_completion_update_sets_ready_for_review() {
        let update = super::completion_success_update();
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::READY_FOR_REVIEW.to_string()));
        assert!(
            update.remove_labels.is_empty()
                || !update
                    .remove_labels
                    .contains(&crate::plan::labels::READY_FOR_REVIEW.to_string())
        );
    }

    #[test]
    fn terminal_completion_update_closes_issue() {
        let update = super::completion_terminal_update("closed");
        assert_eq!(update.status.as_deref(), Some("closed"));
    }

    #[test]
    fn completion_is_superseded_when_matching_orphan_clear_exists() {
        use crate::plan::audit_sentinel::AuditSentinelKind;

        let audits = vec![AuditSentinelKind::DispatchOrphanCleared {
            delegation_id: "del-A".into(),
            reason: crate::server::ORPHAN_CLEAR_REASON_RESTART.into(),
        }];

        assert!(super::completion_is_superseded("del-A", &audits));
        assert!(!super::completion_is_superseded("del-B", &audits));
    }

    #[tokio::test]
    async fn completion_writeback_notifies_fast_forward_channel() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Notify;

        struct NoopPm;

        #[async_trait::async_trait]
        impl PmLike for NoopPm {
            async fn update_issue(
                &self,
                _id: &str,
                _update: spur_pm::IssueUpdate,
            ) -> anyhow::Result<()> {
                Ok(())
            }

            fn closed_status(&self) -> &str {
                "closed"
            }
        }

        let notify = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });
        let result = spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let brain_session_id =
            spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
        let materializer = test_materializer();

        super::persist_worker_completion_and_notify(
            &NoopPm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            &Some(Arc::clone(&notify)),
            &result,
            &brain_session_id,
            1,
            &materializer,
            None,
            None,
        )
        .await
        .expect("persist completion");

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("completion writeback must trigger a fast-forward")
            .expect("waiter task must not panic");
    }

    struct RecordingAdvanced {
        comments: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl spur_pm::BeadsAdvanced for RecordingAdvanced {
        async fn list_ready(
            &self,
            _filter: spur_pm::ReadyFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            Ok(vec![])
        }

        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
            Ok(vec![])
        }

        async fn add_comment(&self, _issue_id: &str, body: &str) -> anyhow::Result<String> {
            let mut comments = self.comments.lock().expect("comments lock");
            comments.push(body.to_string());
            Ok(format!("c{}", comments.len()))
        }

        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
            Ok(vec![])
        }
    }

    struct RecordingPm {
        advanced: RecordingAdvanced,
    }

    #[async_trait::async_trait]
    impl PmLike for RecordingPm {
        async fn update_issue(
            &self,
            _id: &str,
            _update: spur_pm::IssueUpdate,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn closed_status(&self) -> &str {
            "closed"
        }

        fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
            Some(&self.advanced)
        }
    }

    #[tokio::test]
    async fn emit_worker_started_audit_records_branch_session_and_base() {
        let pm = RecordingPm {
            advanced: RecordingAdvanced {
                comments: std::sync::Mutex::new(vec![]),
            },
        };

        super::emit_worker_started_audit(
            Some(&pm),
            &Some("bd-1".to_string()),
            pro_feature_gate().as_ref(),
            "del-A",
            "spur/worker/v2/codex/brain/worker",
            "worker",
            "base-oid",
        )
        .await;

        let comments = pm.advanced.comments.lock().expect("comments lock");
        let body = comments.first().expect("worker-started audit");
        let parsed = crate::plan::audit_sentinel::parse_comment(body)
            .expect("sentinel")
            .expect("parse");
        assert!(matches!(
            parsed,
            crate::plan::audit_sentinel::AuditSentinelKind::WorkerStarted {
                delegation_id,
                worker_branch,
                worker_session_id,
                dispatched_base_oid,
            } if delegation_id == "del-A"
                && worker_branch == "spur/worker/v2/codex/brain/worker"
                && worker_session_id == "worker"
                && dispatched_base_oid == "base-oid"
        ));
    }

    #[tokio::test]
    async fn persist_worker_completion_and_notify_materializes_artifact_uri_in_audit() {
        let pm = RecordingPm {
            advanced: RecordingAdvanced {
                comments: std::sync::Mutex::new(vec![]),
            },
        };
        let result = spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("worker done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-test".into()),
            artifact: None,
        };
        let materializer = test_materializer();
        let brain_session_id =
            spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));

        super::persist_worker_completion_and_notify(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            &None,
            &result,
            &brain_session_id,
            2,
            &materializer,
            None,
            None,
        )
        .await
        .expect("persist completion");

        let comments = pm.advanced.comments.lock().expect("comments lock");
        let completion = comments
            .iter()
            .filter_map(|body| crate::plan::audit_sentinel::parse_comment(body))
            .find_map(|sentinel| match sentinel {
                Ok(crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                    artifact_uri,
                    result_summary,
                    ..
                }) => Some((artifact_uri, result_summary)),
                _ => None,
            })
            .expect("completion audit");

        assert_eq!(
            completion.0.as_deref(),
            Some("spur://outcome/brain-test/del-A/2")
        );
        assert_eq!(completion.1.as_deref(), Some("worker done"));
    }

    #[test]
    fn escalated_to_brain_blocks_recompute_open_statuses_promotion() {
        // bd-2m2u Phase 2d — `recompute_open_statuses` must not promote an
        // EscalatedToBrain task to Ready even when its dependencies are
        // satisfied. Brain `submit_plan_mutation(RetryTask)` is the only
        // way back to Pending → Ready.
        use crate::plan::projector::recompute_open_statuses;

        let mut tasks = vec![
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "dep".into(),
                    agent: "a".into(),
                    task: "T".into(),
                    depends_on: vec![],
                    issue_id: None,
                    issue_title: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "esc".into(),
                    agent: "a".into(),
                    task: "T".into(),
                    depends_on: vec!["dep".into()],
                    issue_id: None,
                    issue_title: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::EscalatedToBrain {
                    last_error: "exhausted".into(),
                },
                result: None,
                worker_branch: None,
                attempt: 2,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
        ];

        recompute_open_statuses(&mut tasks);

        assert!(
            matches!(tasks[1].status, PlanTaskStatus::EscalatedToBrain { .. }),
            "EscalatedToBrain must NOT be auto-promoted to Ready by recompute_open_statuses; got {:?}",
            tasks[1].status
        );
    }

    #[tokio::test]
    async fn apply_issue_update_batches_label_changes_in_single_call() {
        use tokio::sync::Mutex;

        struct RecordingPm {
            calls: Mutex<Vec<(String, spur_pm::IssueUpdate)>>,
        }

        #[async_trait::async_trait]
        impl PmLike for RecordingPm {
            async fn update_issue(
                &self,
                id: &str,
                update: spur_pm::IssueUpdate,
            ) -> anyhow::Result<()> {
                self.calls.lock().await.push((id.to_string(), update));
                Ok(())
            }

            fn closed_status(&self) -> &str {
                "closed"
            }
        }

        let pm = RecordingPm {
            calls: Mutex::new(Vec::new()),
        };

        super::apply_issue_update(
            &pm,
            "bd-1",
            spur_pm::IssueUpdate {
                status: Some("open".to_string()),
                add_labels: vec!["label-a".to_string(), "label-b".to_string()],
                remove_labels: vec!["label-c".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("apply_issue_update must succeed");

        let calls = pm.calls.lock().await;
        assert_eq!(
            calls.len(),
            2,
            "expected one core call + one batched label call, got {calls:?}"
        );

        assert_eq!(calls[0].0, "bd-1");
        assert_eq!(calls[0].1.status, Some("open".to_string()));
        assert!(calls[0].1.add_labels.is_empty());
        assert!(calls[0].1.remove_labels.is_empty());

        assert_eq!(calls[1].0, "bd-1");
        assert_eq!(calls[1].1.status, None);
        assert_eq!(calls[1].1.add_labels, vec!["label-a", "label-b"]);
        assert_eq!(calls[1].1.remove_labels, vec!["label-c"]);
    }

    #[tokio::test]
    async fn request_changes_reuse_prior_worktree_round_trips_attempt_record_and_sentinel() {
        use crate::plan::audit_sentinel::AuditSentinelKind;

        let mut task_spec = task("T1", &[]);
        task_spec.issue_id = Some("bd-1".to_string());
        let entry = PlanTaskEntry {
            spec: task_spec,
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("looks close".into()),
            },
            result: None,
            worker_branch: Some("spur/worker-1".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-1".into()),
            dispatched_base_oid: None,
        };
        let mut state = PlanState {
            plan_id: "p1".into(),
            tasks: vec![entry],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        let outcome = apply_decision_and_extract(
            "p1",
            "T1",
            "request_changes",
            Some("please tighten error handling"),
            true,
            &mut state,
            Some("closed"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("request_changes should succeed");

        let entry = &state.tasks[0];
        assert_eq!(entry.history.len(), 1);
        assert_eq!(entry.history[0].reuse_prior_worktree, Some(true));

        let review_feedback_emit = outcome
            .audit_emits
            .into_iter()
            .find(|emit| matches!(emit, PendingAuditEmit::ReviewFeedback { .. }))
            .expect("review feedback audit emit must be present");
        let ops = review_feedback_emit.into_beads_ops(state.epic_id.as_deref());
        assert!(!ops.is_empty(), "expected at least one beads op");
        let comment = ops[0]
            .update
            .comment
            .as_deref()
            .expect("sentinel comment should be present");
        let parsed = crate::plan::audit_sentinel::parse_comment(comment)
            .expect("sentinel prefix expected")
            .expect("sentinel payload should parse");
        match parsed {
            AuditSentinelKind::ReviewFeedback {
                reuse_prior_worktree,
                ..
            } => assert_eq!(reuse_prior_worktree, Some(true)),
            other => panic!("expected ReviewFeedback sentinel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_changes_reuse_prior_worktree_requires_worker_branch() {
        let entry = PlanTaskEntry {
            spec: task("T1", &[]),
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("wip".into()),
            },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-1".into()),
            dispatched_base_oid: None,
        };
        let mut state = PlanState {
            plan_id: "p1".into(),
            tasks: vec![entry],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };

        let err = apply_decision_and_extract(
            "p1",
            "T1",
            "request_changes",
            Some("retry"),
            true,
            &mut state,
            Some("closed"),
            None,
            None,
            None,
            None,
            None,
        )
        .err()
        .expect("reuse_prior_worktree=true without worker branch must error");

        assert_eq!(
            err,
            "reuse_prior_worktree=true requires a worker_branch on the rejected attempt"
        );
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
                    dispatched_base_oid: None,
                    reuse_prior_worktree: None,
                },
                AttemptRecord {
                    attempt: 2,
                    worker_branch: Some("spur/worker-2".into()),
                    diff_summary: None,
                    summary: None,
                    feedback: "fix that".into(),
                    dispatched_base_oid: None,
                    reuse_prior_worktree: None,
                },
            ],
            last_delegation_id: None,
            dispatched_base_oid: None,
        };
        let mut state = PlanState {
            plan_id: "p1".into(),
            tasks: vec![entry],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };

        let resp = review_task(
            "p1",
            "T1",
            "request_changes",
            Some("please try the other approach"),
            false,
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

    /// Integration test for Phase 0 (RCA bd-2m2u): three Dispatch sentinels
    /// — even when each carries the buggy `attempt: 1` field — must project
    /// to `attempt = 3` via count-based `project_attempt_facts`, which then
    /// trips the `MAX_ATTEMPTS` gate in `review_task(request_changes)`.
    /// Before Phase 0 this gate was dead code in production.
    #[tokio::test]
    async fn request_changes_at_3_dispatches_auto_rejects() {
        use super::*;
        use crate::plan::audit_sentinel::AuditSentinelKind;
        use crate::plan::projector::{project_attempt_facts, project_attempt_history};

        // Three Dispatch sentinels all stamped with the buggy attempt: 1
        // (mimicking what the reconciler emits today). Two ReviewFeedback
        // sentinels for the first two attempts so request_changes is the
        // expected next call after the third dispatch produced AwaitingReview.
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-1".into(),
                attempt: 1,
                feedback: "fix one".into(),
                worker_branch: Some("spur/worker-1".into()),
                summary: Some("partial 1".into()),
                reuse_prior_worktree: None,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-2".into(),
                attempt: 1,
                feedback: "fix two".into(),
                worker_branch: Some("spur/worker-2".into()),
                summary: Some("partial 2".into()),
                reuse_prior_worktree: None,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-3".into(),
                worker: "codex".into(),
                attempt: 1,
            },
        ];

        let (projected_attempt, last_delegation_id) = project_attempt_facts(&audits);
        assert_eq!(
            projected_attempt, 3,
            "count-based projection must yield 3 even though each Dispatch.attempt is 1"
        );
        assert_eq!(last_delegation_id.as_deref(), Some("del-3"));

        let history = project_attempt_history(&audits);
        assert_eq!(history.len(), 2);

        let entry = PlanTaskEntry {
            spec: task("T1", &[]),
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("third attempt awaiting review".into()),
            },
            result: None,
            worker_branch: Some("spur/worker-3".into()),
            attempt: projected_attempt,
            history,
            last_delegation_id,
            dispatched_base_oid: None,
        };
        assert_eq!(entry.attempt, MAX_ATTEMPTS);

        let mut state = PlanState {
            plan_id: "p1".into(),
            tasks: vec![entry],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        };

        let resp = review_task(
            "p1",
            "T1",
            "request_changes",
            Some("please retry"),
            false,
            &mut state,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("MAX_ATTEMPTS reached should auto-reject (Ok), not Err");

        let entry = &state.tasks[0];
        assert!(
            matches!(entry.status, PlanTaskStatus::Rejected { .. }),
            "expected Rejected after 3rd dispatch hits MAX_ATTEMPTS, got {:?}",
            entry.status
        );

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
            "expected MAX_ATTEMPTS auto-reject warning, got {warnings:?}"
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
                    issue_title: None,
                    context_files: vec![],
                },
                status: super::PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: Some("spur/worker-a".into()),
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".into())),
            base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
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
                    issue_title: None,
                    context_files: vec![],
                },
                status: super::PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: Some("spur/worker-a".into()),
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".into())),
            base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
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

    // ─── emit_epic_completion_audit durable-state contract ───────────────

    struct FailingAddCommentAdvanced;

    #[async_trait::async_trait]
    impl spur_pm::BeadsAdvanced for FailingAddCommentAdvanced {
        async fn list_ready(
            &self,
            _filter: spur_pm::ReadyFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            Ok(vec![])
        }

        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
            Ok(vec![])
        }

        async fn add_comment(&self, _issue_id: &str, _body: &str) -> anyhow::Result<String> {
            anyhow::bail!("disk full")
        }

        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn emit_epic_completion_audit_returns_err_when_add_comment_fails() {
        let advanced = FailingAddCommentAdvanced;
        let result = super::emit_epic_completion_audit(
            &advanced,
            "bd-epic",
            "plan-1",
            crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved,
        )
        .await;
        assert!(
            result.is_err(),
            "emit_epic_completion_audit must return Err when add_comment fails"
        );
    }

    // ─── completion writeback audit-first durability contract ───────────

    struct CompletionWritebackAdvanced {
        comments: std::sync::Mutex<Vec<String>>,
        fail_add_comment: bool,
    }

    #[async_trait::async_trait]
    impl spur_pm::BeadsAdvanced for CompletionWritebackAdvanced {
        async fn list_ready(
            &self,
            _filter: spur_pm::ReadyFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            Ok(vec![])
        }

        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
            Ok(self
                .comments
                .lock()
                .expect("comments lock")
                .iter()
                .enumerate()
                .map(|(index, body)| spur_pm::Comment {
                    id: format!("c{}", index + 1),
                    body: body.clone(),
                    actor: "test".to_string(),
                    created_at: chrono::Utc::now(),
                })
                .collect())
        }

        async fn add_comment(&self, _issue_id: &str, body: &str) -> anyhow::Result<String> {
            if self.fail_add_comment {
                anyhow::bail!("comment write failed");
            }
            let mut comments = self.comments.lock().expect("comments lock");
            comments.push(body.to_string());
            Ok(format!("c{}", comments.len()))
        }

        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
            Ok(vec![])
        }
    }

    struct CompletionWritebackPm {
        advanced: CompletionWritebackAdvanced,
        labels: std::sync::Mutex<Vec<String>>,
        status: std::sync::Mutex<Option<String>>,
        updates: std::sync::Mutex<Vec<spur_pm::IssueUpdate>>,
        fail_updates_remaining: std::sync::Mutex<usize>,
    }

    impl CompletionWritebackPm {
        fn new(labels: Vec<String>) -> Self {
            Self {
                advanced: CompletionWritebackAdvanced {
                    comments: std::sync::Mutex::new(Vec::new()),
                    fail_add_comment: false,
                },
                labels: std::sync::Mutex::new(labels),
                status: std::sync::Mutex::new(None),
                updates: std::sync::Mutex::new(Vec::new()),
                fail_updates_remaining: std::sync::Mutex::new(0),
            }
        }

        fn with_comment_failure(labels: Vec<String>) -> Self {
            Self {
                advanced: CompletionWritebackAdvanced {
                    comments: std::sync::Mutex::new(Vec::new()),
                    fail_add_comment: true,
                },
                ..Self::new(labels)
            }
        }

        fn completion_comment_count(&self) -> usize {
            self.advanced
                .comments
                .lock()
                .expect("comments lock")
                .iter()
                .filter(|body| {
                    matches!(
                        crate::plan::audit_sentinel::parse_comment(body),
                        Some(Ok(
                            crate::plan::audit_sentinel::AuditSentinelKind::Completion { .. }
                        ))
                    )
                })
                .count()
        }
    }

    #[async_trait::async_trait]
    impl PmLike for CompletionWritebackPm {
        async fn update_issue(
            &self,
            _id: &str,
            update: spur_pm::IssueUpdate,
        ) -> anyhow::Result<()> {
            self.updates
                .lock()
                .expect("updates lock")
                .push(update.clone());

            {
                let mut remaining = self
                    .fail_updates_remaining
                    .lock()
                    .expect("fail updates lock");
                if *remaining > 0 {
                    *remaining -= 1;
                    anyhow::bail!("issue update failed");
                }
            }

            if let Some(status) = update.status {
                *self.status.lock().expect("status lock") = Some(status);
            }

            let mut labels = self.labels.lock().expect("labels lock");
            for remove in update.remove_labels {
                labels.retain(|label| label != &remove);
            }
            for add in update.add_labels {
                if !labels.contains(&add) {
                    labels.push(add);
                }
            }
            Ok(())
        }

        async fn issue_labels(&self, _id: &str) -> anyhow::Result<Vec<String>> {
            Ok(self.labels.lock().expect("labels lock").clone())
        }

        fn closed_status(&self) -> &str {
            "closed"
        }

        fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
            Some(&self.advanced)
        }
    }

    fn completion_audit_fields() -> crate::plan::audit_sentinel::CompletionAuditFields {
        crate::plan::audit_sentinel::CompletionAuditFields {
            worker_branch: Some("spur/worker-test".to_string()),
            result_summary: Some("worker done".to_string()),
            artifact_uri: None,
            dispatched_base_oid: None,
            repo_root: None,
        }
    }

    fn single_completion_comment(
        pm: &CompletionWritebackPm,
    ) -> crate::plan::audit_sentinel::AuditSentinelKind {
        let comments = pm.advanced.comments.lock().expect("comments lock");
        let completions: Vec<_> = comments
            .iter()
            .filter_map(
                |body| match crate::plan::audit_sentinel::parse_comment(body) {
                    Some(Ok(
                        kind @ crate::plan::audit_sentinel::AuditSentinelKind::Completion { .. },
                    )) => Some(kind),
                    _ => None,
                },
            )
            .collect();
        assert_eq!(
            completions.len(),
            1,
            "expected exactly one completion audit comment, got {completions:?}"
        );
        completions.into_iter().next().unwrap()
    }

    fn run_git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn worker_output_repo(worker_commit_count: usize) -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().expect("temp repo");
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README.md"), "base\n").expect("write base file");
        run_git(dir.path(), &["add", "README.md"]);
        run_git(dir.path(), &["commit", "-q", "-m", "base"]);
        let base = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-b", "worker"]);
        for i in 0..worker_commit_count {
            std::fs::write(
                dir.path().join("worker.txt"),
                format!("worker commit {}\n", i + 1),
            )
            .expect("write worker file");
            run_git(dir.path(), &["add", "worker.txt"]);
            run_git(
                dir.path(),
                &["commit", "-q", "-m", &format!("worker {}", i + 1)],
            );
        }

        (dir, base)
    }

    #[tokio::test]
    async fn awaiting_review_completion_rechecks_audits_before_writing_duplicate() {
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let pm = CompletionWritebackPm::new(vec![delegation_label]);
        pm.advanced.comments.lock().expect("comments lock").push(
            crate::plan::audit_sentinel::encode_comment(
                &crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                    delegation_id: "del-A".into(),
                    completion_state: crate::plan::audit_sentinel::CompletionState::AwaitingReview,
                    superseded: false,
                    worker_branch: Some("spur/worker-test".into()),
                    result_summary: Some("already completed".into()),
                    artifact_uri: None,
                    dispatched_base_oid: None,
                },
            ),
        );
        let seeded_comments = spur_pm::BeadsAdvanced::list_comments(&pm.advanced, "bd-1")
            .await
            .expect("list seeded comments");
        let seeded_audits =
            crate::plan::projector::collect_sorted_audits_for_issue("bd-1", seeded_comments);
        assert!(
            super::completion_audit_already_emitted("del-A", &seeded_audits),
            "test precondition must expose the seeded completion audit"
        );

        let action = super::persist_completion_result_with_retry_for_task(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            completion_audit_fields(),
            false,
            Some(1),
            Some("t1"),
        )
        .await
        .expect("idempotent completion recheck");

        assert_eq!(action, super::CompletionPersistenceAction::AlreadyCompleted);
        assert_eq!(
            pm.completion_comment_count(),
            1,
            "guard must not append a duplicate completion audit"
        );
        assert!(
            pm.updates.lock().expect("updates lock").is_empty(),
            "guard must return before applying label/status updates"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_returns_err_when_add_comment_fails() {
        let pm = CompletionWritebackPm::with_comment_failure(vec![]);

        let result = super::emit_completion_audit(
            Some(&pm),
            &Some("bd-1".to_string()),
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            completion_audit_fields(),
        )
        .await;

        assert!(
            result.is_err(),
            "emit_completion_audit must return Err when add_comment fails"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_unlicensed_returns_ok() {
        let pm = CompletionWritebackPm::new(vec![]);

        let result = super::emit_completion_audit(
            Some(&pm),
            &Some("bd-1".to_string()),
            unlicensed_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            completion_audit_fields(),
        )
        .await;

        assert!(
            result.is_ok(),
            "unlicensed completion audit fallback must not fail writeback"
        );
        assert_eq!(
            pm.advanced.comments.lock().expect("comments lock").len(),
            0,
            "unlicensed fallback must not call add_comment"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_populates_dispatched_base_oid() {
        let (repo, base) = worker_output_repo(1);
        let pm = CompletionWritebackPm::new(vec![]);
        let entry = PlanTaskEntry {
            spec: PlanTask {
                task_id: "T1".into(),
                agent: "codex".into(),
                task: "Do T1".into(),
                depends_on: Vec::new(),
                issue_id: Some("bd-1".into()),
                issue_title: None,
                context_files: Vec::new(),
            },
            status: PlanTaskStatus::AwaitingReview { summary: None },
            result: None,
            worker_branch: Some("worker".to_string()),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: Some("del-A".to_string()),
            dispatched_base_oid: Some(base.clone()),
        };

        super::emit_completion_audit(
            Some(&pm),
            &entry.spec.issue_id,
            pro_feature_gate().as_ref(),
            "plan-1",
            entry.last_delegation_id.as_deref().unwrap(),
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: entry.worker_branch.clone(),
                dispatched_base_oid: entry.dispatched_base_oid.clone(),
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
        )
        .await
        .expect("emit completion audit");

        let comments = pm.advanced.comments.lock().expect("comments lock");
        let body = comments.first().expect("completion audit comment");
        assert!(
            body.contains(&format!("\"dispatched_base_oid\":\"{base}\"")),
            "completion audit body should carry dispatched_base_oid: {body}"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_accepts_single_commit_worker_output() {
        let (repo, base) = worker_output_repo(1);
        let pm = CompletionWritebackPm::new(vec![]);

        let result = super::emit_completion_audit(
            Some(&pm),
            &Some("bd-1".to_string()),
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("worker".to_string()),
                dispatched_base_oid: Some(base),
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
        )
        .await;

        assert!(result.is_ok(), "single-commit worker output must pass");
        assert_eq!(
            pm.completion_comment_count(),
            1,
            "valid worker output should emit the completion audit"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_preserves_awaiting_review_for_multi_commit_output() {
        let (repo, base) = worker_output_repo(2);
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let lease_label = crate::plan::labels::lease_expires_at(1_777_777_777);
        let pm = CompletionWritebackPm::new(vec![delegation_label.clone(), lease_label.clone()]);

        super::persist_completion_result_after_worker_output_invariant(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("worker".to_string()),
                dispatched_base_oid: Some(base.clone()),
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
            false,
        )
        .await
        .expect("persist completion");

        match single_completion_comment(&pm) {
            crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                completion_state,
                worker_branch,
                result_summary,
                dispatched_base_oid,
                ..
            } => {
                assert_eq!(
                    completion_state,
                    crate::plan::audit_sentinel::CompletionState::AwaitingReview
                );
                assert_eq!(worker_branch.as_deref(), Some("worker"));
                assert_eq!(dispatched_base_oid.as_deref(), Some(base.as_str()));
                assert_eq!(result_summary.as_deref(), Some("worker done"));
            }
            other => panic!("expected completion audit, got {other:?}"),
        }

        assert_eq!(
            pm.status.lock().expect("status lock").as_deref(),
            None,
            "awaiting-review completion should not close the issue"
        );
        let labels = pm.labels.lock().expect("labels lock");
        assert!(
            !labels.contains(&delegation_label),
            "delegation label must be cleared after completion write"
        );
        assert!(
            !labels.contains(&lease_label),
            "lease label must be cleared after downgrade"
        );
        assert!(
            labels.contains(&crate::plan::labels::READY_FOR_REVIEW.to_string()),
            "awaiting review completion must add ready-for-review"
        );
    }

    #[tokio::test]
    async fn zero_commit_output_does_not_auto_retry_after_normalization_design() {
        let (repo, base) = worker_output_repo(0);
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let lease_label = crate::plan::labels::lease_expires_at(1_777_777_777);
        let pm = CompletionWritebackPm::new(vec![delegation_label.clone(), lease_label.clone()]);

        let action = super::persist_completion_result_after_worker_output_invariant_with_retry(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("worker".to_string()),
                dispatched_base_oid: Some(base),
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
            false,
            1,
            Some("t1"),
        )
        .await
        .expect("persist retryable invariant violation");

        assert!(matches!(
            action,
            super::CompletionPersistenceAction::Completed(
                crate::plan::audit_sentinel::CompletionState::AwaitingReview
            )
        ));
        assert_eq!(
            pm.status.lock().expect("status lock").as_deref(),
            None,
            "awaiting-review completion should leave issue status unchanged"
        );

        let comments = pm.advanced.comments.lock().expect("comments lock");
        let retry = comments
            .iter()
            .filter_map(|body| crate::plan::audit_sentinel::parse_comment(body))
            .filter_map(Result::ok)
            .find_map(|kind| match kind {
                crate::plan::audit_sentinel::AuditSentinelKind::RetryRequested {
                    error,
                    worker_branch,
                    ..
                } => Some((error, worker_branch)),
                _ => None,
            })
            .is_some();
        assert!(!retry, "normalization design must not emit RetryRequested");
    }

    #[derive(Default)]
    struct RecordingPlanEventSink {
        events: std::sync::Mutex<Vec<spur_acp::SpurEventBody>>,
    }

    impl crate::events::McpEventSink for RecordingPlanEventSink {
        fn emit(&self, event: spur_acp::SpurEventBody) {
            self.events.lock().expect("events lock").push(event);
        }
    }

    #[tokio::test]
    async fn auto_retry_completion_emits_event_without_brain_continuation() {
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let pm = CompletionWritebackPm::new(vec![delegation_label]);
        let result = DelegationResult {
            status: DelegationStatus::Failed {
                error: "worker crashed".to_string(),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-failed".to_string()),
            artifact: None,
        };

        let deferred = super::persist_worker_completion_and_notify(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            &None,
            &result,
            &spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            1,
            test_materializer().as_ref(),
            None,
            Some("task-1"),
        )
        .await
        .expect("auto-retry completion should persist")
        .expect("auto-retry completion should defer an event");

        let continuation_count = Arc::new(AtomicUsize::new(0));
        let continuation_count_for_ctx = Arc::clone(&continuation_count);
        let continuation_ctx = crate::server::DetachedContinuationCtx {
            on_complete: Arc::new(move |_, _| {
                continuation_count_for_ctx.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            }),
        };
        let sink = RecordingPlanEventSink::default();

        deferred.deliver(Some(&sink), &continuation_ctx).await;

        assert_eq!(
            continuation_count.load(Ordering::SeqCst),
            0,
            "auto-retry should not re-prompt the brain"
        );
        let events = sink.events.lock().expect("events lock");
        assert_eq!(events.len(), 1);
        match &events[0] {
            spur_acp::SpurEventBody::PlanTaskAutoRetried {
                plan_id,
                task_id,
                delegation_id,
                attempt,
                max_attempts,
                error,
                worker_branch,
            } => {
                assert_eq!(plan_id, "plan-1");
                assert_eq!(task_id, "task-1");
                assert_eq!(delegation_id, "del-A");
                assert_eq!(*attempt, 1);
                assert_eq!(*max_attempts, MAX_ATTEMPTS);
                assert_eq!(error, "worker crashed");
                assert_eq!(worker_branch.as_deref(), Some("spur/worker-failed"));
            }
            other => panic!("expected PlanTaskAutoRetried event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_completion_audit_skips_invariant_for_failed_state() {
        let (repo, base) = worker_output_repo(2);
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let pm = CompletionWritebackPm::new(vec![delegation_label.clone()]);

        super::persist_completion_result_after_worker_output_invariant(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::Failed,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("worker".to_string()),
                result_summary: Some("worker failed before first commit".to_string()),
                dispatched_base_oid: Some(base),
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
            false,
        )
        .await
        .expect("persist failed completion");

        match single_completion_comment(&pm) {
            crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                completion_state,
                result_summary,
                ..
            } => {
                assert_eq!(
                    completion_state,
                    crate::plan::audit_sentinel::CompletionState::Failed
                );
                assert_eq!(
                    result_summary.as_deref(),
                    Some("worker failed before first commit"),
                    "Failed completions should not get the AwaitingReview invariant diagnostic"
                );
            }
            other => panic!("expected completion audit, got {other:?}"),
        }

        assert_eq!(
            pm.status.lock().expect("status lock").as_deref(),
            Some("closed"),
            "Failed completion should keep the terminal update"
        );
        let labels = pm.labels.lock().expect("labels lock");
        assert!(
            !labels.contains(&delegation_label),
            "delegation label must be cleared for Failed completion"
        );
    }

    #[tokio::test]
    async fn emit_completion_audit_skips_commit_count_without_dispatched_base_oid() {
        let (repo, _base) = worker_output_repo(2);
        let pm = CompletionWritebackPm::new(vec![]);

        let result = super::emit_completion_audit(
            Some(&pm),
            &Some("bd-1".to_string()),
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("worker".to_string()),
                dispatched_base_oid: None,
                repo_root: Some(repo.path().to_path_buf()),
                ..completion_audit_fields()
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "legacy completion without dispatched_base_oid should skip the invariant"
        );
        assert_eq!(
            pm.completion_comment_count(),
            1,
            "legacy completion should still emit the audit"
        );
    }

    #[tokio::test]
    async fn persist_completion_result_audit_failure_aborts_mutation() {
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let pm = CompletionWritebackPm::with_comment_failure(vec![delegation_label.clone()]);

        let result = super::persist_completion_result(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            completion_audit_fields(),
            false,
        )
        .await;

        assert!(
            result.is_err(),
            "audit failure must abort completion writeback"
        );
        assert!(
            pm.status.lock().expect("status lock").is_none(),
            "status must not change after audit failure"
        );
        let labels = pm.labels.lock().expect("labels lock");
        assert!(
            labels.contains(&delegation_label),
            "delegation label must remain for retry after audit failure"
        );
        assert!(
            !labels.contains(&crate::plan::labels::READY_FOR_REVIEW.to_string()),
            "ready-for-review must not be added after audit failure"
        );
        assert!(
            pm.updates.lock().expect("updates lock").is_empty(),
            "issue mutation must not run after audit failure"
        );
    }

    #[tokio::test]
    async fn persist_completion_result_idempotent_after_audit_success_mutate_failure() {
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let pm = CompletionWritebackPm::new(vec![delegation_label.clone()]);
        *pm.fail_updates_remaining.lock().expect("fail updates lock") = 1;

        let first = super::persist_completion_result(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            completion_audit_fields(),
            false,
        )
        .await;

        assert!(first.is_err(), "first mutation failure must be returned");
        assert_eq!(
            pm.completion_comment_count(),
            1,
            "first attempt should write one durable completion audit"
        );

        let retry = super::persist_completion_result(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            completion_audit_fields(),
            true,
        )
        .await;

        assert!(
            retry.is_ok(),
            "retry must complete after audit was already emitted"
        );
        assert_eq!(
            pm.completion_comment_count(),
            1,
            "retry with already_emitted=true must not duplicate completion audit"
        );
        let labels = pm.labels.lock().expect("labels lock");
        assert!(
            labels.contains(&crate::plan::labels::READY_FOR_REVIEW.to_string()),
            "retry must re-apply ready-for-review"
        );
        assert!(
            !labels.contains(&delegation_label),
            "retry must clear delegation label"
        );
    }

    #[tokio::test]
    async fn persist_completion_result_combined_update_clears_lease_and_delegation_label_in_one_pass(
    ) {
        let delegation_label = crate::plan::labels::delegation_id("del-A");
        let lease_label = crate::plan::labels::lease_expires_at(1_777_777_777);
        let pm = CompletionWritebackPm::new(vec![delegation_label.clone(), lease_label.clone()]);

        super::persist_completion_result(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-1",
            "del-A",
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            completion_audit_fields(),
            false,
        )
        .await
        .expect("persist completion");

        let updates = pm.updates.lock().expect("updates lock");
        let label_update = updates
            .iter()
            .find(|update| {
                update
                    .add_labels
                    .contains(&crate::plan::labels::READY_FOR_REVIEW.to_string())
            })
            .expect("ready-for-review label update");
        assert!(
            label_update.remove_labels.contains(&delegation_label),
            "delegation label must be removed in the same IssueUpdate"
        );
        assert!(
            label_update.remove_labels.contains(&lease_label),
            "lease label must be removed in the same IssueUpdate"
        );
    }

    // ─── bd-2m2u Phase 2d — persisted-path escalation tests ──────────────────

    #[tokio::test]
    async fn escalated_task_keeps_beads_issue_open_and_signal_escalated_label() {
        // Phase 2d invariant: when `persist_completion_result_with_retry_for_task`
        // observes attempt-2 worker failure (i.e. retry budget exhausted),
        // it must (a) keep the beads issue OPEN, (b) add the
        // `signal:escalated` label, (c) emit an `EscalationRequested`
        // audit, and (d) return `Escalated` rather than completing.
        let pm = CompletionWritebackPm::new(vec![]);

        let action = super::persist_completion_result_with_retry_for_task(
            &pm,
            "bd-1",
            pro_feature_gate().as_ref(),
            "plan-esc",
            "del-esc",
            crate::plan::audit_sentinel::CompletionState::Failed,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some("spur/worker-bust".into()),
                result_summary: Some("worker crashed".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
                repo_root: None,
            },
            false,
            Some(2),
            Some("t-esc"),
        )
        .await
        .expect("persist completion (escalation path)");

        match action {
            super::CompletionPersistenceAction::Escalated {
                ref last_error,
                ref worker_branch,
            } => {
                assert_eq!(last_error, "worker crashed");
                assert_eq!(worker_branch.as_deref(), Some("spur/worker-bust"));
            }
            other => panic!("expected CompletionPersistenceAction::Escalated, got {other:?}"),
        }

        let status = pm.status.lock().expect("status lock").clone();
        assert_eq!(
            status.as_deref(),
            Some("open"),
            "escalation must keep beads issue OPEN; got {status:?}"
        );

        let labels = pm.labels.lock().expect("labels lock").clone();
        assert!(
            labels
                .iter()
                .any(|l| l == crate::plan::mutation_executor::SIGNAL_ESCALATED_LABEL),
            "escalation must add `signal:escalated` label; got labels={labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l == crate::plan::labels::READY_FOR_REVIEW),
            "escalation must remove `READY_FOR_REVIEW` (option A: SignalWatcher should not pick it up); got labels={labels:?}"
        );

        let comments = pm.advanced.comments.lock().expect("comments lock").clone();
        let kinds: Vec<_> = comments
            .iter()
            .filter_map(|body| {
                crate::plan::audit_sentinel::parse_comment(body).and_then(|r| r.ok())
            })
            .collect();
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                crate::plan::audit_sentinel::AuditSentinelKind::EscalationRequested {
                    plan_id, task_id, attempt, last_error, ..
                }
                if plan_id == "plan-esc" && task_id == "t-esc" && *attempt == 2 && last_error == "worker crashed"
            )),
            "EscalationRequested audit (plan_id=plan-esc, task_id=t-esc, attempt=2) must be emitted; got {kinds:?}"
        );
    }
}
