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
use spur_pm::{IssueFilter, PmService};
use uuid::Uuid;

use super::mutation_executor::apply_mutation;
use super::proposers::{MutationProposer, MutationScorer};
use super::signals::{parse_comment, SENTINEL_PREFIX};

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
