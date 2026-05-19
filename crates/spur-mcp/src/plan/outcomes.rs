use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

pub const TRANSITION_RING_CAP: usize = 64;
pub const STUCK_DURATION: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DispatchOutcome {
    Dispatched {
        task_id: String,
        agent: String,
        delegation_id: String,
        agent_fallback: bool,
        timestamp: SystemTime,
    },
    Skipped {
        task_id: String,
        reason: SkipReason,
        timestamp: SystemTime,
    },
    NoReadyTasks {
        plan_id: String,
        reason: NoReadyReason,
        timestamp: SystemTime,
    },
    NoDispatchContext {
        plan_id: Option<String>,
        ready_count: usize,
        timestamp: SystemTime,
    },
}

impl DispatchOutcome {
    pub fn timestamp(&self) -> SystemTime {
        match self {
            Self::Dispatched { timestamp, .. }
            | Self::Skipped { timestamp, .. }
            | Self::NoReadyTasks { timestamp, .. }
            | Self::NoDispatchContext { timestamp, .. } => *timestamp,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkipReason {
    MissingPlanId,
    TaskMissingFromProjection,
    TaskStatusNotReady {
        blocked_by: Vec<String>,
    },
    PlanMissingCompleteEpic,
    PlanHasPendingEpic,
    PlanOwnedByAnotherBrain {
        owner: String,
    },
    EpicNotOpen,
    MissingIssueId,
    DuplicateIssueId,
    ProjectorFailed {
        error: String,
    },
    PersistError {
        msg: String,
    },
    DispatchSendFailed {
        msg: String,
    },
    MissingDispatchLeaseExpiry,
    UnsupportedReadyIssueType {
        issue_type: Option<String>,
    },
    BaseSpecBuildFailed {
        error: String,
    },
    PredispatchOverlayConflict {
        dep_task_id: String,
        files: Vec<String>,
    },
    PersistDispatchIntentFailed {
        error: String,
    },
    HydrationGetIssueFailed {
        error: String,
    },
    PlanAllowsDispatchFailed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NoReadyReason {
    NoMatchingRows,
    ProjectorBehind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StuckTask {
    pub plan_id: String,
    pub task_id: String,
    pub reason: SkipReason,
    pub stuck_since: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcilerStatus {
    pub recent_outcomes: Vec<DispatchOutcome>,
    pub stuck_tasks: Vec<StuckTask>,
    pub last_tick_at: Option<SystemTime>,
    pub last_tick_plans_enumerated: usize,
    pub last_tick_plans_dispatched: usize,
}

/// Ephemeral reconciler outcomes. MUST NOT be persisted to beads; reconstruct
/// durable plan state from beads on restart.
#[derive(Debug, Clone)]
pub struct OutcomeBuffer {
    pub latest_per_task: HashMap<String, DispatchOutcome>,
    pub transition_ring: VecDeque<DispatchOutcome>,
    transition_ring_cap: usize,
}

impl Default for OutcomeBuffer {
    fn default() -> Self {
        Self::with_capacity(TRANSITION_RING_CAP)
    }
}

impl OutcomeBuffer {
    pub fn with_capacity(transition_ring_cap: usize) -> Self {
        Self {
            latest_per_task: HashMap::new(),
            transition_ring: VecDeque::with_capacity(transition_ring_cap),
            transition_ring_cap,
        }
    }

    pub fn record(&mut self, outcome: DispatchOutcome) {
        match &outcome {
            DispatchOutcome::Dispatched { task_id, .. } => {
                self.latest_per_task.insert(task_id.clone(), outcome);
            }
            DispatchOutcome::Skipped {
                task_id, reason, ..
            } => {
                if matches!(reason, SkipReason::MissingPlanId) {
                    self.push_transition(outcome);
                } else {
                    self.latest_per_task.insert(task_id.clone(), outcome);
                }
            }
            DispatchOutcome::NoReadyTasks { .. } | DispatchOutcome::NoDispatchContext { .. } => {
                self.push_transition(outcome);
            }
        }
    }

    pub fn snapshot(&self) -> Vec<DispatchOutcome> {
        let mut outcomes =
            Vec::with_capacity(self.latest_per_task.len() + self.transition_ring.len());
        outcomes.extend(self.latest_per_task.values().cloned());
        outcomes.extend(self.transition_ring.iter().cloned());
        outcomes.sort_by_key(DispatchOutcome::timestamp);
        outcomes
    }

    pub fn latest(&self, task_id: &str) -> Option<&DispatchOutcome> {
        self.latest_per_task.get(task_id)
    }

    fn push_transition(&mut self, outcome: DispatchOutcome) {
        if self.transition_ring_cap == 0 {
            return;
        }
        if self.transition_ring.len() == self.transition_ring_cap {
            self.transition_ring.pop_front();
        }
        self.transition_ring.push_back(outcome);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeLogDecision {
    pub state_changed: bool,
    pub stuck_warn: Option<StuckTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkipObservation {
    key: SkipReasonKey,
    reason: SkipReason,
    first_seen_at: SystemTime,
    warned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SkipReasonKey {
    MissingPlanId,
    TaskMissingFromProjection,
    TaskStatusNotReady { blocked_by: Vec<String> },
    PlanMissingCompleteEpic,
    PlanHasPendingEpic,
    PlanOwnedByAnotherBrain,
    EpicNotOpen,
    MissingIssueId,
    DuplicateIssueId,
    ProjectorFailed,
    PersistError,
    DispatchSendFailed,
    MissingDispatchLeaseExpiry,
    UnsupportedReadyIssueType,
    BaseSpecBuildFailed,
    PredispatchOverlayConflict,
    PersistDispatchIntentFailed,
    HydrationGetIssueFailed,
    PlanAllowsDispatchFailed,
}

impl From<&SkipReason> for SkipReasonKey {
    fn from(reason: &SkipReason) -> Self {
        match reason {
            SkipReason::MissingPlanId => Self::MissingPlanId,
            SkipReason::TaskMissingFromProjection => Self::TaskMissingFromProjection,
            SkipReason::TaskStatusNotReady { blocked_by } => Self::TaskStatusNotReady {
                blocked_by: sorted_unique(blocked_by),
            },
            SkipReason::PlanMissingCompleteEpic => Self::PlanMissingCompleteEpic,
            SkipReason::PlanHasPendingEpic => Self::PlanHasPendingEpic,
            SkipReason::PlanOwnedByAnotherBrain { .. } => Self::PlanOwnedByAnotherBrain,
            SkipReason::EpicNotOpen => Self::EpicNotOpen,
            SkipReason::MissingIssueId => Self::MissingIssueId,
            SkipReason::DuplicateIssueId => Self::DuplicateIssueId,
            SkipReason::ProjectorFailed { .. } => Self::ProjectorFailed,
            SkipReason::PersistError { .. } => Self::PersistError,
            SkipReason::DispatchSendFailed { .. } => Self::DispatchSendFailed,
            SkipReason::MissingDispatchLeaseExpiry => Self::MissingDispatchLeaseExpiry,
            SkipReason::UnsupportedReadyIssueType { .. } => Self::UnsupportedReadyIssueType,
            SkipReason::BaseSpecBuildFailed { .. } => Self::BaseSpecBuildFailed,
            SkipReason::PredispatchOverlayConflict { .. } => Self::PredispatchOverlayConflict,
            SkipReason::PersistDispatchIntentFailed { .. } => Self::PersistDispatchIntentFailed,
            SkipReason::HydrationGetIssueFailed { .. } => Self::HydrationGetIssueFailed,
            SkipReason::PlanAllowsDispatchFailed { .. } => Self::PlanAllowsDispatchFailed,
        }
    }
}

fn sorted_unique(values: &[String]) -> Vec<String> {
    values
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Ephemeral outcome store owned by the plan engine. MUST NOT be persisted to
/// beads; reconstruct durable plan state from beads on restart.
#[derive(Debug, Clone, Default)]
pub struct OutcomeStore {
    /// Ephemeral, MUST NOT be persisted to beads; per-plan latest outcomes.
    pub outcomes_by_plan: HashMap<String, OutcomeBuffer>,
    /// Ephemeral, MUST NOT be persisted to beads; global/orphan outcomes.
    pub outcomes_global: OutcomeBuffer,
    skip_observations: HashMap<(String, String), SkipObservation>,
    last_tick_at: Option<SystemTime>,
    last_tick_plans_enumerated: usize,
    last_tick_plans_dispatched: usize,
}

impl OutcomeStore {
    pub fn mark_tick(&mut self, now: SystemTime) {
        self.last_tick_at = Some(now);
        self.last_tick_plans_enumerated = 0;
        self.last_tick_plans_dispatched = 0;
    }

    pub fn record_tick_plans_enumerated(&mut self, count: usize) {
        self.last_tick_plans_enumerated += count;
    }

    pub fn record_outcome(&mut self, plan_id: Option<&str>, outcome: DispatchOutcome) {
        match plan_id {
            Some(plan_id) => self
                .outcomes_by_plan
                .entry(plan_id.to_string())
                .or_default()
                .record(outcome),
            None => self.outcomes_global.record(outcome),
        }
    }

    pub fn prune_task(&mut self, plan_id: &str, task_id: &str) {
        if let Some(buffer) = self.outcomes_by_plan.get_mut(plan_id) {
            buffer.latest_per_task.remove(task_id);
        }
        self.skip_observations
            .remove(&(plan_id.to_string(), task_id.to_string()));
    }

    pub fn drop_plan(&mut self, plan_id: &str) {
        self.outcomes_by_plan.remove(plan_id);
        self.skip_observations.retain(|(p, _), _| p != plan_id);
    }

    pub fn record_no_ready(
        &mut self,
        plan_id: &str,
        reason: NoReadyReason,
        now: SystemTime,
    ) -> DispatchOutcome {
        let outcome = DispatchOutcome::NoReadyTasks {
            plan_id: plan_id.to_string(),
            reason,
            timestamp: now,
        };
        self.record_outcome(Some(plan_id), outcome.clone());
        outcome
    }

    pub fn record_no_dispatch_context(
        &mut self,
        plan_id: Option<&str>,
        ready_count: usize,
        now: SystemTime,
    ) -> DispatchOutcome {
        let outcome = DispatchOutcome::NoDispatchContext {
            plan_id: plan_id.map(str::to_string),
            ready_count,
            timestamp: now,
        };
        self.record_outcome(plan_id, outcome.clone());
        outcome
    }

    pub fn record_dispatched(
        &mut self,
        plan_id: &str,
        task_id: &str,
        agent: &str,
        delegation_id: &str,
        agent_fallback: bool,
        now: SystemTime,
    ) -> DispatchOutcome {
        self.skip_observations
            .remove(&(plan_id.to_string(), task_id.to_string()));
        let outcome = DispatchOutcome::Dispatched {
            task_id: task_id.to_string(),
            agent: agent.to_string(),
            delegation_id: delegation_id.to_string(),
            agent_fallback,
            timestamp: now,
        };
        self.record_outcome(Some(plan_id), outcome.clone());
        self.last_tick_plans_dispatched += 1;
        outcome
    }

    pub fn record_skipped(
        &mut self,
        plan_id: Option<&str>,
        task_id: &str,
        reason: SkipReason,
        now: SystemTime,
    ) -> OutcomeLogDecision {
        let plan_key = plan_id.unwrap_or("").to_string();
        let task_key = task_id.to_string();
        let observation_key = (plan_key.clone(), task_key.clone());
        let reason_key = SkipReasonKey::from(&reason);

        let mut decision = OutcomeLogDecision {
            state_changed: false,
            stuck_warn: None,
        };

        match self.skip_observations.get_mut(&observation_key) {
            Some(observation) if observation.key == reason_key => {
                if !observation.warned
                    && now
                        .duration_since(observation.first_seen_at)
                        .unwrap_or_default()
                        >= STUCK_DURATION
                {
                    observation.warned = true;
                    decision.stuck_warn = Some(StuckTask {
                        plan_id: plan_key.clone(),
                        task_id: task_key.clone(),
                        reason: observation.reason.clone(),
                        stuck_since: observation.first_seen_at,
                    });
                }
                observation.reason = reason.clone();
            }
            _ => {
                decision.state_changed = true;
                self.skip_observations.insert(
                    observation_key,
                    SkipObservation {
                        key: reason_key,
                        reason: reason.clone(),
                        first_seen_at: now,
                        warned: false,
                    },
                );
            }
        }

        let outcome = DispatchOutcome::Skipped {
            task_id: task_id.to_string(),
            reason,
            timestamp: now,
        };
        self.record_outcome(plan_id, outcome);
        decision
    }

    pub fn recent_outcomes(&self, plan_id: &str) -> Vec<DispatchOutcome> {
        self.outcomes_by_plan
            .get(plan_id)
            .map(OutcomeBuffer::snapshot)
            .unwrap_or_default()
    }

    pub fn global_recent_outcomes(&self) -> Vec<DispatchOutcome> {
        let mut outcomes = self.outcomes_global.snapshot();
        outcomes.extend(
            self.outcomes_by_plan
                .values()
                .flat_map(OutcomeBuffer::snapshot),
        );
        outcomes.sort_by_key(DispatchOutcome::timestamp);
        outcomes
    }

    pub fn stuck_tasks(&self) -> Vec<StuckTask> {
        self.skip_observations
            .iter()
            .filter_map(|((plan_id, task_id), observation)| {
                if observation.warned {
                    Some(StuckTask {
                        plan_id: plan_id.clone(),
                        task_id: task_id.clone(),
                        reason: observation.reason.clone(),
                        stuck_since: observation.first_seen_at,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn stuck_tasks_for_plan(&self, plan_id: &str) -> Vec<StuckTask> {
        self.stuck_tasks()
            .into_iter()
            .filter(|task| task.plan_id == plan_id)
            .collect()
    }

    pub fn reconciler_status(&self) -> ReconcilerStatus {
        ReconcilerStatus {
            recent_outcomes: self.global_recent_outcomes(),
            stuck_tasks: self.stuck_tasks(),
            last_tick_at: self.last_tick_at,
            last_tick_plans_enumerated: self.last_tick_plans_enumerated,
            last_tick_plans_dispatched: self.last_tick_plans_dispatched,
        }
    }

    pub fn skip_observations_len(&self) -> usize {
        self.skip_observations.len()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn ts(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn skip(task_id: &str, reason: SkipReason, seconds: u64) -> DispatchOutcome {
        DispatchOutcome::Skipped {
            task_id: task_id.into(),
            reason,
            timestamp: ts(seconds),
        }
    }

    macro_rules! skip_reason_records_in_latest {
        ($name:ident, $reason:expr) => {
            #[test]
            fn $name() {
                let mut store = OutcomeStore::default();
                let expected = $reason;
                store.record_skipped(Some("P1"), "task-1", expected.clone(), ts(1));

                let outcomes = store.recent_outcomes("P1");
                match outcomes.as_slice() {
                    [DispatchOutcome::Skipped { reason, .. }] => assert_eq!(reason, &expected),
                    other => panic!("expected one skipped outcome, got {other:?}"),
                }
            }
        };
    }

    skip_reason_records_in_latest!(
        skip_reason_task_missing_from_projection_records,
        SkipReason::TaskMissingFromProjection
    );
    skip_reason_records_in_latest!(
        skip_reason_task_status_not_ready_records_blockers,
        SkipReason::TaskStatusNotReady {
            blocked_by: vec!["bd-a".into(), "bd-b".into()]
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_plan_missing_complete_epic_records,
        SkipReason::PlanMissingCompleteEpic
    );
    skip_reason_records_in_latest!(
        skip_reason_plan_has_pending_epic_records,
        SkipReason::PlanHasPendingEpic
    );
    skip_reason_records_in_latest!(skip_reason_epic_not_open_records, SkipReason::EpicNotOpen);
    skip_reason_records_in_latest!(
        skip_reason_missing_issue_id_records,
        SkipReason::MissingIssueId
    );
    skip_reason_records_in_latest!(
        skip_reason_duplicate_issue_id_records,
        SkipReason::DuplicateIssueId
    );
    skip_reason_records_in_latest!(
        skip_reason_projector_failed_records_without_error_in_key,
        SkipReason::ProjectorFailed {
            error: "projection failed".into()
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_persist_error_records_without_message_in_key,
        SkipReason::PersistError { msg: "boom".into() }
    );
    skip_reason_records_in_latest!(
        skip_reason_dispatch_send_failed_records,
        SkipReason::DispatchSendFailed {
            msg: "closed".into()
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_missing_dispatch_lease_expiry_records,
        SkipReason::MissingDispatchLeaseExpiry
    );
    skip_reason_records_in_latest!(
        skip_reason_unsupported_ready_issue_type_records,
        SkipReason::UnsupportedReadyIssueType {
            issue_type: Some("feature".into())
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_base_spec_build_failed_records_without_error_in_key,
        SkipReason::BaseSpecBuildFailed {
            error: "base spec build failed".into()
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_persist_dispatch_intent_failed_records_without_error_in_key,
        SkipReason::PersistDispatchIntentFailed {
            error: "persist intent failed".into()
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_hydration_get_issue_failed_records_without_error_in_key,
        SkipReason::HydrationGetIssueFailed {
            error: "get_issue failed".into()
        }
    );
    skip_reason_records_in_latest!(
        skip_reason_plan_allows_dispatch_failed_records_without_error_in_key,
        SkipReason::PlanAllowsDispatchFailed {
            error: "plan_allows_dispatch failed".into()
        }
    );

    #[test]
    fn skip_reason_base_spec_build_failed_dedup_collapses_distinct_errors() {
        let mut store = OutcomeStore::default();
        let first = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::BaseSpecBuildFailed {
                error: "first".into(),
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::BaseSpecBuildFailed {
                error: "second".into(),
            },
            ts(2),
        );

        assert!(first.state_changed);
        assert!(!second.state_changed);
    }

    #[test]
    fn skip_reason_persist_dispatch_intent_failed_dedup_collapses_distinct_errors() {
        let mut store = OutcomeStore::default();
        let first = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::PersistDispatchIntentFailed {
                error: "first".into(),
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::PersistDispatchIntentFailed {
                error: "second".into(),
            },
            ts(2),
        );

        assert!(first.state_changed);
        assert!(!second.state_changed);
    }

    #[test]
    fn skip_reason_hydration_get_issue_failed_dedup_collapses_distinct_errors() {
        let mut store = OutcomeStore::default();
        let first = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::HydrationGetIssueFailed {
                error: "first".into(),
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::HydrationGetIssueFailed {
                error: "second".into(),
            },
            ts(2),
        );

        assert!(first.state_changed);
        assert!(!second.state_changed);
    }

    #[test]
    fn skip_reason_plan_allows_dispatch_failed_dedup_collapses_distinct_errors() {
        let mut store = OutcomeStore::default();
        let first = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::PlanAllowsDispatchFailed {
                error: "first".into(),
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::PlanAllowsDispatchFailed {
                error: "second".into(),
            },
            ts(2),
        );

        assert!(first.state_changed);
        assert!(!second.state_changed);
    }

    #[test]
    fn skip_reason_missing_plan_id_records_as_global_transition() {
        let mut store = OutcomeStore::default();
        store.record_skipped(None, "orphan", SkipReason::MissingPlanId, ts(1));

        assert!(matches!(
            store.reconciler_status().recent_outcomes.as_slice(),
            [DispatchOutcome::Skipped {
                reason: SkipReason::MissingPlanId,
                ..
            }]
        ));
    }

    #[test]
    fn buffer_routes_missing_plan_id_skip_to_transition_ring() {
        let mut buffer = OutcomeBuffer::default();
        buffer.record(skip("orphan-epic", SkipReason::MissingPlanId, 1));

        assert!(buffer.latest("orphan-epic").is_none());
        assert_eq!(buffer.snapshot().len(), 1);
    }

    #[test]
    fn buffer_routes_task_skip_to_latest_per_task() {
        let mut buffer = OutcomeBuffer::default();
        buffer.record(skip(
            "task-1",
            SkipReason::TaskStatusNotReady {
                blocked_by: vec!["bd-a".into()],
            },
            1,
        ));

        assert!(matches!(
            buffer.latest("task-1"),
            Some(DispatchOutcome::Skipped { .. })
        ));
        assert_eq!(buffer.snapshot().len(), 1);
    }

    #[test]
    fn buffer_routes_no_ready_to_transition_ring() {
        let mut buffer = OutcomeBuffer::default();
        buffer.record(DispatchOutcome::NoReadyTasks {
            plan_id: "P1".into(),
            reason: NoReadyReason::NoMatchingRows,
            timestamp: ts(1),
        });

        assert_eq!(buffer.snapshot().len(), 1);
        assert!(buffer.latest("P1").is_none());
    }

    #[test]
    fn snapshot_sorts_oldest_to_newest_across_routes() {
        let mut buffer = OutcomeBuffer::default();
        buffer.record(skip("task-late", SkipReason::MissingIssueId, 30));
        buffer.record(DispatchOutcome::NoDispatchContext {
            plan_id: Some("P1".into()),
            ready_count: 2,
            timestamp: ts(10),
        });
        buffer.record(skip("task-early", SkipReason::DuplicateIssueId, 20));

        let stamps: Vec<SystemTime> = buffer
            .snapshot()
            .into_iter()
            .map(|outcome| outcome.timestamp())
            .collect();
        assert_eq!(stamps, vec![ts(10), ts(20), ts(30)]);
    }

    #[test]
    fn latest_per_task_overwrites_duplicate_key() {
        let mut buffer = OutcomeBuffer::default();
        buffer.record(skip("task-1", SkipReason::MissingIssueId, 1));
        buffer.record(skip("task-1", SkipReason::DuplicateIssueId, 2));

        assert!(matches!(
            buffer.latest("task-1"),
            Some(DispatchOutcome::Skipped {
                reason: SkipReason::DuplicateIssueId,
                ..
            })
        ));
        assert_eq!(buffer.snapshot().len(), 1);
    }

    #[test]
    fn transition_ring_capacity_rolls_over_oldest_entries() {
        let mut buffer = OutcomeBuffer::with_capacity(3);
        for second in 1..=5 {
            buffer.record(DispatchOutcome::NoReadyTasks {
                plan_id: format!("P{second}"),
                reason: NoReadyReason::NoMatchingRows,
                timestamp: ts(second),
            });
        }

        let stamps: Vec<SystemTime> = buffer
            .snapshot()
            .into_iter()
            .map(|outcome| outcome.timestamp())
            .collect();
        assert_eq!(stamps, vec![ts(3), ts(4), ts(5)]);
    }

    #[test]
    fn same_skip_reason_emits_info_once() {
        let mut store = OutcomeStore::default();
        let first = store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(1));
        let second = store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(2));

        assert!(first.state_changed);
        assert!(!second.state_changed);
    }

    #[test]
    fn reason_variant_change_is_state_change() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(1));
        let second =
            store.record_skipped(Some("P1"), "task-1", SkipReason::DuplicateIssueId, ts(2));

        assert!(second.state_changed);
    }

    #[test]
    fn blocker_set_order_does_not_change_state() {
        let mut store = OutcomeStore::default();
        store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::TaskStatusNotReady {
                blocked_by: vec!["bd-b".into(), "bd-a".into()],
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::TaskStatusNotReady {
                blocked_by: vec!["bd-a".into(), "bd-b".into()],
            },
            ts(2),
        );

        assert!(!second.state_changed);
    }

    #[test]
    fn blocker_set_change_is_state_change() {
        let mut store = OutcomeStore::default();
        store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::TaskStatusNotReady {
                blocked_by: vec!["bd-a".into(), "bd-b".into()],
            },
            ts(1),
        );
        let second = store.record_skipped(
            Some("P1"),
            "task-1",
            SkipReason::TaskStatusNotReady {
                blocked_by: vec!["bd-b".into()],
            },
            ts(2),
        );

        assert!(second.state_changed);
    }

    #[test]
    fn stuck_warn_uses_wall_clock_and_fires_once() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(0));
        let early = store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(60));
        let threshold =
            store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(121));
        let repeated =
            store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(300));

        assert!(early.stuck_warn.is_none());
        assert!(threshold.stuck_warn.is_some());
        assert!(repeated.stuck_warn.is_none());
    }

    #[test]
    fn stuck_warn_refires_after_reason_change() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(0));
        assert!(store
            .record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(121))
            .stuck_warn
            .is_some());
        store.record_skipped(Some("P1"), "task-1", SkipReason::DuplicateIssueId, ts(130));

        assert!(store
            .record_skipped(Some("P1"), "task-1", SkipReason::DuplicateIssueId, ts(251))
            .stuck_warn
            .is_some());
    }

    #[test]
    fn dispatch_resets_stuck_tracking() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(0));
        assert!(store
            .record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(121))
            .stuck_warn
            .is_some());
        store.record_dispatched("P1", "task-1", "codex", "del-1", false, ts(130));

        assert!(
            store
                .record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(131))
                .state_changed
        );
    }

    #[test]
    fn cross_plan_status_returns_stuck_tasks_from_each_plan() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(0));
        store.record_skipped(Some("P2"), "task-2", SkipReason::DuplicateIssueId, ts(0));
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(121));
        store.record_skipped(Some("P2"), "task-2", SkipReason::DuplicateIssueId, ts(121));

        let status = store.reconciler_status();
        assert_eq!(status.stuck_tasks.len(), 2);
        assert!(status.stuck_tasks.iter().any(|task| task.plan_id == "P1"));
        assert!(status.stuck_tasks.iter().any(|task| task.plan_id == "P2"));
    }

    #[test]
    fn cross_plan_recent_outcomes_keep_same_task_id_per_plan() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task", SkipReason::MissingIssueId, ts(1));
        store.record_skipped(Some("P2"), "task", SkipReason::DuplicateIssueId, ts(2));

        let status = store.reconciler_status();
        assert_eq!(status.recent_outcomes.len(), 2);
        assert!(matches!(
            status.recent_outcomes.as_slice(),
            [
                DispatchOutcome::Skipped {
                    reason: SkipReason::MissingIssueId,
                    ..
                },
                DispatchOutcome::Skipped {
                    reason: SkipReason::DuplicateIssueId,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn orphan_outcome_is_global_not_per_plan() {
        let mut store = OutcomeStore::default();
        store.record_skipped(None, "orphan", SkipReason::MissingPlanId, ts(1));

        assert_eq!(store.reconciler_status().recent_outcomes.len(), 1);
        assert!(store.recent_outcomes("P1").is_empty());
    }

    #[test]
    fn prune_task_is_noop_for_absent_plan_or_task() {
        let mut store = OutcomeStore::default();
        store.record_dispatched("P1", "task-1", "codex", "del-1", false, ts(1));
        store.record_no_dispatch_context(None, 1, ts(2));

        store.prune_task("missing-plan", "task-1");
        store.prune_task("P1", "missing-task");

        assert!(matches!(
            store
                .outcomes_by_plan
                .get("P1")
                .and_then(|buffer| buffer.latest("task-1")),
            Some(DispatchOutcome::Dispatched { .. })
        ));
        assert_eq!(store.outcomes_global.snapshot().len(), 1);
    }

    #[test]
    fn prune_task_removes_only_that_task_from_that_plan() {
        let mut store = OutcomeStore::default();
        store.record_dispatched("P1", "task-1", "codex", "del-1", false, ts(1));
        store.record_dispatched("P1", "task-2", "codex", "del-2", false, ts(2));
        store.record_dispatched("P2", "task-1", "codex", "del-3", false, ts(3));
        store.record_no_dispatch_context(None, 1, ts(4));

        store.prune_task("P1", "task-1");

        let p1 = store.outcomes_by_plan.get("P1").expect("P1 buffer");
        assert!(p1.latest("task-1").is_none());
        assert!(matches!(
            p1.latest("task-2"),
            Some(DispatchOutcome::Dispatched { .. })
        ));
        assert!(matches!(
            store
                .outcomes_by_plan
                .get("P2")
                .and_then(|buffer| buffer.latest("task-1")),
            Some(DispatchOutcome::Dispatched { .. })
        ));
        assert_eq!(store.outcomes_global.snapshot().len(), 1);
    }

    #[test]
    fn drop_plan_is_noop_for_absent_plan() {
        let mut store = OutcomeStore::default();
        store.record_dispatched("P1", "task-1", "codex", "del-1", false, ts(1));
        store.record_no_dispatch_context(None, 1, ts(2));

        store.drop_plan("missing-plan");

        assert!(store.outcomes_by_plan.contains_key("P1"));
        assert_eq!(store.outcomes_global.snapshot().len(), 1);
    }

    #[test]
    fn drop_plan_removes_only_that_plan_and_preserves_global() {
        let mut store = OutcomeStore::default();
        store.record_dispatched("P1", "task-1", "codex", "del-1", false, ts(1));
        store.record_dispatched("P2", "task-1", "codex", "del-2", false, ts(2));
        store.record_no_dispatch_context(None, 1, ts(3));

        store.drop_plan("P1");

        assert!(!store.outcomes_by_plan.contains_key("P1"));
        assert!(store.outcomes_by_plan.contains_key("P2"));
        assert_eq!(store.outcomes_global.snapshot().len(), 1);
    }

    #[test]
    fn prune_task_removes_skip_observation_for_target_task_only() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(1));
        store.record_skipped(Some("P1"), "task-2", SkipReason::DuplicateIssueId, ts(2));

        store.prune_task("P1", "task-1");

        assert!(!store
            .skip_observations
            .contains_key(&("P1".to_string(), "task-1".to_string())));
        assert!(store
            .skip_observations
            .contains_key(&("P1".to_string(), "task-2".to_string())));
    }

    #[test]
    fn drop_plan_clears_all_skip_observations_for_plan_only() {
        let mut store = OutcomeStore::default();
        store.record_skipped(Some("P1"), "task-1", SkipReason::MissingIssueId, ts(1));
        store.record_skipped(Some("P1"), "task-2", SkipReason::DuplicateIssueId, ts(2));
        store.record_skipped(Some("P2"), "task-1", SkipReason::MissingIssueId, ts(3));

        store.drop_plan("P1");

        assert!(!store.skip_observations.keys().any(|(plan, _)| plan == "P1"));
        assert!(store
            .skip_observations
            .contains_key(&("P2".to_string(), "task-1".to_string())));
    }

    #[test]
    fn last_tick_at_is_reported() {
        let mut store = OutcomeStore::default();
        store.mark_tick(ts(42));

        assert_eq!(store.reconciler_status().last_tick_at, Some(ts(42)));
    }
}
