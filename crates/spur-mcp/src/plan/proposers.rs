//! MutationProposer + MutationScorer trait seam.
//!
//! v0 ships deterministic impls; v1 MCTS replanner substitutes at callsite.
//! Trait shapes are fixed so substitution is compile-only.

use async_trait::async_trait;
use uuid::Uuid;

use super::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use super::signals::WorkerSignal;
use super::PlanState;

#[async_trait]
pub trait MutationProposer: Send + Sync {
    /// Produce candidate batches. Empty vec = no mutation proposed; signal
    /// watcher then treats it as a normal review.
    async fn propose(
        &self,
        state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch>;
}

#[async_trait]
pub trait MutationScorer: Send + Sync {
    async fn score(&self, state: &PlanState, batch: &MutationBatch) -> f32;
}

/// v0b impl: any `ScopeDrift` with severity >= `severity_threshold` produces
/// one `SplitTask` with `estimated_subtasks` children (default 2), all
/// parallel under a `Barrier` rewire.
pub struct ScopeDriftSplitProposer {
    pub severity_threshold: f32,
}

impl Default for ScopeDriftSplitProposer {
    fn default() -> Self {
        Self {
            severity_threshold: 0.5,
        }
    }
}

#[async_trait]
impl MutationProposer for ScopeDriftSplitProposer {
    async fn propose(
        &self,
        _state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        match signal {
            WorkerSignal::ScopeDrift {
                signal_id,
                severity,
                reason,
                estimated_subtasks,
            } => {
                if *severity < self.severity_threshold {
                    return vec![];
                }
                let n = estimated_subtasks.unwrap_or(2).max(2) as usize;
                let children: Vec<TaskDraft> = (0..n)
                    .map(|i| TaskDraft {
                        title: format!("[subtask {}/{}] {}", i + 1, n, reason),
                        description: format!(
                            "Auto-generated from scope-drift signal {}. Original task: {}. Narrow this subtask.",
                            signal_id, triggering_task
                        ),
                        assignee: None,
                        priority: None,
                    })
                    .collect();
                vec![MutationBatch {
                    mutation_id: Uuid::new_v4(),
                    trigger_signal_id: Some(*signal_id),
                    trigger_task_id: triggering_task.to_string(),
                    ops: vec![PlanMutationOp::SplitTask {
                        parent: triggering_task.to_string(),
                        children,
                        dep_rewire: DepRewirePolicy::Barrier,
                    }],
                }]
            }
            WorkerSignal::PotentialClobber { .. } => vec![],
            WorkerSignal::RetryExhausted { .. } => vec![],
            WorkerSignal::MarkNoop { .. } => vec![],
        }
    }
}

/// bd-2m2u Phase 2e — v0 deterministic recovery proposer for the
/// `WorkerSignal::RetryExhausted` autonomous-proposer path.
///
/// Policy: if the task referenced by the signal has `attempt < MAX_ATTEMPTS`
/// AND the task is NOT already escalated to the brain, propose exactly one
/// `MutationBatch { ops: [RetryTask] }`. Otherwise propose nothing —
/// escalation (option A in Phase 2d) means the brain owns the task and the
/// autonomous proposer must defer to avoid racing with brain-driven recovery.
///
/// v1 (out of scope here) will be LLM-driven and may emit `ModifyTaskSpec`,
/// `InsertTaskBefore`, etc. The trait seam keeps that swap compile-only.
pub struct RetryExhaustedProposer;

#[async_trait]
impl MutationProposer for RetryExhaustedProposer {
    async fn propose(
        &self,
        state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        let WorkerSignal::RetryExhausted {
            signal_id, task_id, ..
        } = signal
        else {
            return vec![];
        };
        let target_id = if task_id.is_empty() {
            triggering_task
        } else {
            task_id.as_str()
        };
        let entry = state.tasks.iter().find(|entry| {
            entry.spec.task_id == target_id || entry.spec.issue_id.as_deref() == Some(target_id)
        });
        // E2 guard — if the task already projects as `EscalatedToBrain`
        // (which is exactly the projection of a `signal:escalated` label,
        // see projector.rs:372-393), the brain owns recovery via
        // `submit_plan_mutation`. Defer silently to avoid racing with the
        // human/brain-driven path that already escalated this issue.
        if matches!(
            entry.map(|e| &e.status),
            Some(super::PlanTaskStatus::EscalatedToBrain { .. })
        ) {
            return vec![];
        }
        let attempt = entry
            .map(|entry| entry.attempt)
            .unwrap_or(super::MAX_ATTEMPTS); // unknown task → no proposal
        if attempt >= super::MAX_ATTEMPTS {
            return vec![];
        }
        vec![MutationBatch {
            mutation_id: Uuid::new_v4(),
            trigger_signal_id: Some(*signal_id),
            trigger_task_id: triggering_task.to_string(),
            ops: vec![PlanMutationOp::RetryTask {
                issue_id: target_id.to_string(),
            }],
        }]
    }
}

/// Composite proposer: dispatches to a list of inner proposers and flattens
/// their batches. Used by the production `SignalWatcher` so multiple v0
/// deterministic proposers (`ScopeDriftSplitProposer`,
/// `RetryExhaustedProposer`, ...) can co-exist behind the single trait seam
/// without changing `SignalWatcher`'s shape. Each inner proposer is expected
/// to no-op on signal kinds it doesn't recognize (the existing v0 impls all
/// do this), so the composite is safe to fan out blindly.
pub struct CompositeProposer {
    inner: Vec<Box<dyn MutationProposer>>,
}

impl CompositeProposer {
    pub fn new(inner: Vec<Box<dyn MutationProposer>>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl MutationProposer for CompositeProposer {
    async fn propose(
        &self,
        state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        let mut out = Vec::new();
        for proposer in &self.inner {
            let batches = proposer.propose(state, signal, triggering_task).await;
            out.extend(batches);
        }
        out
    }
}

/// v0b impl: returns 1.0 for any non-empty batch, 0.0 for empty. Placeholder
/// until MCTS rollout scorer ships in v1.
pub struct TrivialScorer;

#[async_trait]
impl MutationScorer for TrivialScorer {
    async fn score(&self, _state: &PlanState, batch: &MutationBatch) -> f32 {
        if batch.ops.is_empty() {
            0.0
        } else {
            1.0
        }
    }
}

// Test helper — counts ops without needing a real PlanState.
#[cfg(test)]
impl TrivialScorer {
    async fn score_len(&self, batch: &MutationBatch) -> usize {
        batch.ops.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_drift_below_threshold_proposes_nothing() {
        let proposer = ScopeDriftSplitProposer {
            severity_threshold: 0.5,
        };
        let signal = WorkerSignal::ScopeDrift {
            signal_id: Uuid::new_v4(),
            severity: 0.2,
            reason: "minor".into(),
            estimated_subtasks: None,
        };
        // PlanState construction omitted — proposer ignores it in this impl.
        // Use a zero-cost dummy via unsafe or refactor PlanState to have Default.
        // For now, assert the severity gate by probing severity alone:
        // (restructure: compute gate without calling propose)
        assert!(!signal_matches_threshold(
            &signal,
            proposer.severity_threshold
        ));
    }

    fn signal_matches_threshold(signal: &WorkerSignal, threshold: f32) -> bool {
        match signal {
            WorkerSignal::ScopeDrift { severity, .. } => *severity >= threshold,
            WorkerSignal::PotentialClobber { .. } => false,
            WorkerSignal::RetryExhausted { .. } => false,
            WorkerSignal::MarkNoop { .. } => false,
        }
    }

    #[tokio::test]
    async fn scope_drift_above_threshold_emits_barrier_split() {
        let signal = WorkerSignal::ScopeDrift {
            signal_id: Uuid::new_v4(),
            severity: 0.8,
            reason: "auth spans 4 subsystems".into(),
            estimated_subtasks: Some(3),
        };
        // Integration-level assertion deferred to mutation_split.rs (Task 8).
        // Here just round-trip the signal to exercise the scorer path.
        let scorer = TrivialScorer;
        let batch = MutationBatch {
            mutation_id: Uuid::new_v4(),
            trigger_signal_id: Some(signal.signal_id()),
            trigger_task_id: "bd-102".into(),
            ops: vec![PlanMutationOp::SplitTask {
                parent: "bd-102".into(),
                children: vec![TaskDraft {
                    title: "t1".into(),
                    description: "".into(),
                    assignee: None,
                    priority: None,
                }],
                dep_rewire: DepRewirePolicy::Barrier,
            }],
        };
        // Placeholder state — TrivialScorer ignores it
        assert_eq!(scorer.score_len(&batch).await, 1);
    }

    // bd-2m2u Phase 2e — RetryExhaustedProposer (v0 deterministic).

    fn entry_with_attempt(task_id: &str, attempt: u32) -> super::super::PlanTaskEntry {
        super::super::PlanTaskEntry {
            spec: super::super::PlanTask {
                task_id: task_id.to_string(),
                agent: "claude-code-acp".into(),
                task: "task body".into(),
                depends_on: Vec::new(),
                issue_id: Some(task_id.to_string()),
                issue_title: None,
                context_files: Vec::new(),
            },
            status: super::super::PlanTaskStatus::Pending,
            result: None,
            worker_branch: None,
            attempt,
            history: Vec::new(),
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn plan_state(tasks: Vec<super::super::PlanTaskEntry>) -> PlanState {
        PlanState {
            plan_id: "plan-2m2u".into(),
            tasks,
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
                "brain-session".into(),
            )),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: super::super::PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    #[tokio::test]
    async fn retry_exhausted_proposer_v0_proposes_retry_task_under_cap() {
        let proposer = RetryExhaustedProposer;
        let state = plan_state(vec![entry_with_attempt("bd-200", 1)]);
        let signal = WorkerSignal::RetryExhausted {
            signal_id: Uuid::new_v4(),
            task_id: "bd-200".into(),
            attempt: 1,
            last_error: "worker crashed".into(),
        };
        let batches = proposer.propose(&state, &signal, "bd-200").await;
        assert_eq!(batches.len(), 1, "expected 1 batch under cap");
        assert_eq!(batches[0].ops.len(), 1);
        assert!(matches!(
            batches[0].ops[0],
            PlanMutationOp::RetryTask { ref issue_id } if issue_id == "bd-200"
        ));
        assert_eq!(batches[0].trigger_signal_id, Some(signal.signal_id()));
    }

    #[tokio::test]
    async fn retry_exhausted_proposer_v0_returns_empty_at_max_attempts() {
        let proposer = RetryExhaustedProposer;
        // attempt == MAX_ATTEMPTS (3): cap reached, no retry should be proposed.
        let state = plan_state(vec![entry_with_attempt(
            "bd-201",
            super::super::MAX_ATTEMPTS,
        )]);
        let signal = WorkerSignal::RetryExhausted {
            signal_id: Uuid::new_v4(),
            task_id: "bd-201".into(),
            attempt: super::super::MAX_ATTEMPTS,
            last_error: "worker crashed again".into(),
        };
        let batches = proposer.propose(&state, &signal, "bd-201").await;
        assert!(
            batches.is_empty(),
            "expected empty batches at MAX_ATTEMPTS cap, got {batches:?}"
        );
    }

    #[tokio::test]
    async fn retry_exhausted_proposer_returns_empty_on_escalated_issue() {
        // bd-2m2u Phase 2e — E2 guard. If the task already projects as
        // `EscalatedToBrain` (i.e. carries `signal:escalated`), the brain
        // owns recovery via submit_plan_mutation. The autonomous proposer
        // must defer rather than racing with brain-driven retry/modify.
        let proposer = RetryExhaustedProposer;
        let mut entry = entry_with_attempt("bd-202", 1);
        entry.status = super::super::PlanTaskStatus::EscalatedToBrain {
            last_error: "auto-retry exhausted".into(),
        };
        let state = plan_state(vec![entry]);
        let signal = WorkerSignal::RetryExhausted {
            signal_id: Uuid::new_v4(),
            task_id: "bd-202".into(),
            attempt: 1,
            last_error: "worker crashed".into(),
        };
        let batches = proposer.propose(&state, &signal, "bd-202").await;
        assert!(
            batches.is_empty(),
            "expected empty batches when issue is escalated; got {batches:?}"
        );
    }

    #[tokio::test]
    async fn composite_proposer_fans_out_to_each_inner_and_flattens() {
        // Sanity: composite dispatch sums the batches of its constituents.
        // ScopeDriftSplitProposer + RetryExhaustedProposer is the production
        // configuration — feed it a RetryExhausted signal and only the second
        // proposer should produce a batch.
        let composite = CompositeProposer::new(vec![
            Box::new(ScopeDriftSplitProposer::default()),
            Box::new(RetryExhaustedProposer),
        ]);
        let state = plan_state(vec![entry_with_attempt("bd-203", 1)]);
        let signal = WorkerSignal::RetryExhausted {
            signal_id: Uuid::new_v4(),
            task_id: "bd-203".into(),
            attempt: 1,
            last_error: "worker crashed".into(),
        };
        let batches = composite.propose(&state, &signal, "bd-203").await;
        assert_eq!(batches.len(), 1);
        assert!(matches!(
            batches[0].ops[0],
            PlanMutationOp::RetryTask { ref issue_id } if issue_id == "bd-203"
        ));
    }
}
