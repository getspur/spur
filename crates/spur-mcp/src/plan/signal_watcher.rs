//! Brain-side signal watcher: polls open (non-closed) tasks bearing a
//! `signal:*` label and no `spur:signal-processed:*` label, dedupes by
//! `signal_id`, invokes a `MutationProposer` + `MutationScorer`, and applies
//! the highest-scored batch. Status filter uses `PmService::closed_status()`
//! per I5 (beads vocabulary compression); the pre-I5 description of
//! "`awaiting_review` tasks" was pre-shipping framing — beads never persists
//! that SPUR-vocab string.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use spur_pm::{IssueFilter, IssueUpdate, PmService};
use uuid::Uuid;

use super::audit_sentinel::{self, AuditSentinelKind};
use super::mutation_executor::apply_mutation;
use super::proposers::{MutationProposer, MutationScorer};
use super::signals::{parse_comment, WorkerSignal, SENTINEL_PREFIX};

pub struct SignalWatcher<P: MutationProposer, S: MutationScorer> {
    pm: Arc<PmService>,
    proposer: P,
    scorer: S,
    seen: Mutex<HashSet<Uuid>>,
    tick: Duration,
    feature_gate: Arc<spur_license::FeatureGate>,
}

impl<P: MutationProposer, S: MutationScorer> SignalWatcher<P, S> {
    pub fn new(
        pm: Arc<PmService>,
        proposer: P,
        scorer: S,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> Self {
        Self {
            pm,
            proposer,
            scorer,
            seen: Mutex::new(HashSet::new()),
            tick: Duration::from_secs(3),
            feature_gate,
        }
    }

    pub async fn run(self, cancel: tokio::sync::oneshot::Receiver<()>) {
        tokio::pin!(cancel);

        loop {
            tokio::select! {
                _ = &mut cancel => {
                    tracing::info!("signal watcher received cancel");
                    break;
                }
                _ = tokio::time::sleep(self.tick) => {}
            }

            tokio::select! {
                biased;
                _ = &mut cancel => {
                    tracing::info!("signal watcher received cancel during tick");
                    break;
                }
                result = self.tick_once() => {
                    if let Err(error) = result {
                        tracing::warn!("signal watcher tick failed: {error}");
                    }
                }
            }
        }
    }

    pub async fn tick_once(&self) -> anyhow::Result<()> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let adv = self
            .pm
            .advanced()
            .ok_or_else(|| anyhow::anyhow!("signal watcher requires beads backend"))?;

        let candidates = self.pm.list_issues(IssueFilter::default()).await?;
        let closed_status = self.pm.closed_status();

        for issue in candidates {
            // Beads persists a compressed status vocabulary: SPUR's nine-state
            // PlanTaskStatus projects to open / closed in the backend. Skip
            // closed tasks — their mutations are already committed or they are
            // otherwise terminal; any signal arriving now is a late arrival
            // (handled by handle_report_signal, not the watcher).
            if issue.status.as_str() == closed_status {
                continue;
            }
            if !issue
                .labels
                .iter()
                .any(|label| label.starts_with("signal:"))
            {
                continue;
            }
            let has_ready_label = issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::READY_FOR_REVIEW);
            if !has_ready_label {
                continue;
            }
            if issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::REVIEW_REJECTED)
            {
                continue;
            }
            let mut comments = adv.list_comments(&issue.id).await?;
            comments.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            let audits = crate::plan::projector::collect_sorted_audits_for_issue(
                &issue.id,
                comments.clone(),
            )?;
            if !crate::plan::projector::awaiting_review_from_audits(&audits) {
                crate::plan::projector::emit_label_audit_drift(
                    "ready-for-review",
                    "label_only",
                    &issue.id,
                );
                continue;
            }

            for comment in comments {
                if !comment.body.trim_start().starts_with(SENTINEL_PREFIX) {
                    continue;
                }

                let signal = match parse_comment(&comment.body) {
                    Some(Ok(signal)) => signal,
                    Some(Err(error)) => {
                        tracing::warn!(
                            issue_id = %issue.id,
                            comment_id = %comment.id,
                            "signal watcher skipped malformed signal sentinel: {error}"
                        );
                        continue;
                    }
                    None => continue,
                };

                let signal_id = signal.signal_id();
                let processed_label = crate::plan::labels::signal_processed_label(&signal_id);
                if issue.labels.iter().any(|label| label == &processed_label) {
                    continue;
                }
                if self.seen.lock().contains(&signal_id) {
                    continue;
                }

                let plan_id = issue
                    .labels
                    .iter()
                    .find_map(|label| crate::plan::labels::parse_plan_id(label))
                    .ok_or_else(|| {
                        anyhow::anyhow!("signal task {} missing spur:plan-id label", issue.id)
                    })?;
                if let WorkerSignal::Escalate { reason, .. } = &signal {
                    let (attempt, delegation_id, worker_branch) = escalation_audit_context(&audits);
                    let task_id = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
                        .unwrap_or_else(|| issue.id.clone());
                    let audit = AuditSentinelKind::EscalationRequested {
                        plan_id: plan_id.to_string(),
                        task_id,
                        attempt,
                        last_error: reason.clone(),
                        worker_branch,
                        delegation_id,
                    };
                    self.pm
                        .update_issue(
                            &issue.id,
                            IssueUpdate {
                                status: Some("open".to_string()),
                                comment: Some(audit_sentinel::encode_comment(&audit)),
                                add_labels: vec![
                                    crate::plan::labels::SIGNAL_ESCALATED.to_string(),
                                    processed_label,
                                ],
                                remove_labels: vec![
                                    crate::plan::labels::READY_FOR_REVIEW.to_string(),
                                    "ready-for-review".to_string(),
                                ],
                                ..Default::default()
                            },
                        )
                        .await?;
                    self.seen.lock().insert(signal_id);
                    break;
                }
                let state = crate::plan::projector::project_plan_from_beads(
                    self.pm.as_ref(),
                    plan_id,
                    self.feature_gate.as_ref(),
                )
                .await?;
                let mut scored_batches = Vec::new();
                for batch in self.proposer.propose(&state, &signal, &issue.id).await {
                    let score = self.scorer.score(&state, &batch).await;
                    scored_batches.push((score, batch));
                }

                scored_batches
                    .sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));

                // Mark `seen` only on a decisive outcome: successful apply, or no
                // proposer candidates (re-running an empty proposer every tick is
                // wasteful). A failed `apply_mutation` leaves `seen` untouched so
                // the next tick retries the signal — crucial for transient PM
                // errors, which would otherwise suppress retry for the process
                // lifetime. Pairs with the durable `spur:signal-processed:<uuid>`
                // label written by the executor on commit for cross-tick dedup.
                match scored_batches.into_iter().next() {
                    Some((_score, batch)) => match apply_mutation(
                        self.pm.clone(),
                        Arc::clone(&self.feature_gate),
                        &batch,
                    )
                    .await
                    {
                        Ok(_) => {
                            self.seen.lock().insert(signal_id);
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                issue_id = %issue.id,
                                %signal_id,
                                "signal watcher failed to apply mutation; will retry next tick: {error}"
                            );
                            break;
                        }
                    },
                    None => {
                        self.seen.lock().insert(signal_id);
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

fn escalation_audit_context(audits: &[AuditSentinelKind]) -> (u32, Option<String>, Option<String>) {
    let mut attempt = 0;
    let mut delegation_id = None;
    let mut worker_branch = None;

    for audit in audits {
        match audit {
            AuditSentinelKind::Dispatch {
                delegation_id: current_delegation_id,
                attempt: current_attempt,
                ..
            } => {
                attempt = *current_attempt;
                delegation_id = Some(current_delegation_id.clone());
                worker_branch = None;
            }
            AuditSentinelKind::Completion {
                delegation_id: current_delegation_id,
                worker_branch: current_worker_branch,
                ..
            } => {
                delegation_id = Some(current_delegation_id.clone());
                if current_worker_branch.is_some() {
                    worker_branch = current_worker_branch.clone();
                }
            }
            _ => {}
        }
    }

    (attempt, delegation_id, worker_branch)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
    use crate::plan::mutation::MutationBatch;
    use crate::plan::signals::{encode_comment as encode_signal, WorkerSignal};

    struct PanicProposer;

    #[async_trait]
    impl MutationProposer for PanicProposer {
        async fn propose(
            &self,
            _state: &crate::plan::PlanState,
            _signal: &WorkerSignal,
            _triggering_task: &str,
        ) -> Vec<MutationBatch> {
            panic!("escalate signals must not invoke mutation proposers");
        }
    }

    struct PanicScorer;

    #[async_trait]
    impl MutationScorer for PanicScorer {
        async fn score(&self, _state: &crate::plan::PlanState, _batch: &MutationBatch) -> f32 {
            panic!("escalate signals must not invoke mutation scoring");
        }
    }

    #[tokio::test]
    async fn escalate_signal_routes_to_brain_escalation_without_proposer() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join(".beads")).expect("create .beads");
        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, dir.path(), None)
                .await
                .expect("pm service init")
                .expect("beads pm service"),
        );
        let task_id = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Escalate me".into(),
                description: Some("task body".into()),
                issue_type: Some("task".into()),
                labels: vec![
                    crate::plan::labels::plan_id("plan-watch"),
                    crate::plan::labels::plan_task_id("task-escalate"),
                    crate::plan::labels::READY_FOR_REVIEW.to_string(),
                    crate::plan::labels::signal_kind("escalate"),
                ],
                ..Default::default()
            })
            .await
            .expect("create task issue");
        let adv = pm.advanced().expect("advanced beads surface");
        adv.add_comment(
            &task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
                delegation_id: "del-watch".into(),
                worker: "codex".into(),
                attempt: 2,
            }),
        )
        .await
        .expect("dispatch audit");
        adv.add_comment(
            &task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: "del-watch".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-watch".into()),
                result_summary: Some("worker paused".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            }),
        )
        .await
        .expect("completion audit");
        let signal_id = Uuid::new_v4();
        let reason = "architecture needs brain confirmation".to_string();
        adv.add_comment(
            &task_id,
            &encode_signal(&WorkerSignal::Escalate {
                signal_id,
                reason: reason.clone(),
            }),
        )
        .await
        .expect("signal comment");

        let watcher = SignalWatcher::new(
            Arc::clone(&pm),
            PanicProposer,
            PanicScorer,
            crate::server::pro_feature_gate(),
        );

        watcher.tick_once().await.expect("watcher tick");

        let issue = pm.get_issue(&task_id).await.expect("updated issue");
        assert!(
            !issue
                .labels
                .contains(&crate::plan::labels::READY_FOR_REVIEW.to_string()),
            "escalation must remove ready-for-review label; labels={:?}",
            issue.labels
        );
        assert!(
            issue
                .labels
                .contains(&crate::plan::labels::SIGNAL_ESCALATED.to_string()),
            "escalation must add signal:escalated label; labels={:?}",
            issue.labels
        );
        assert!(
            issue
                .labels
                .contains(&crate::plan::labels::signal_processed_label(&signal_id)),
            "escalation must mark signal processed; labels={:?}",
            issue.labels
        );

        let comments = adv.list_comments(&task_id).await.expect("comments");
        let audits = comments
            .iter()
            .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert!(
            audits.iter().any(|audit| matches!(
                audit,
                AuditSentinelKind::EscalationRequested {
                    plan_id,
                    task_id,
                    attempt,
                    last_error,
                    worker_branch,
                    delegation_id,
                } if plan_id == "plan-watch"
                    && task_id == "task-escalate"
                    && *attempt == 2
                    && last_error == &reason
                    && worker_branch.as_deref() == Some("spur/worker-watch")
                    && delegation_id.as_deref() == Some("del-watch")
            )),
            "watcher must emit EscalationRequested audit; audits={audits:?}"
        );
    }
}
