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
use spur_acp::{BrainSessionId, SessionId};
use spur_pm::{IssueFilter, PmService};
use uuid::Uuid;

use super::mutation_executor::apply_mutation;
use super::proposers::{MutationProposer, MutationScorer};
use super::signals::{parse_comment, SENTINEL_PREFIX};
use super::PlanState;

pub struct SignalWatcher<P: MutationProposer, S: MutationScorer> {
    pm: Arc<PmService>,
    proposer: P,
    scorer: S,
    seen: Mutex<HashSet<Uuid>>,
    tick: Duration,
}

impl<P: MutationProposer, S: MutationScorer> SignalWatcher<P, S> {
    pub fn new(pm: Arc<PmService>, proposer: P, scorer: S) -> Self {
        Self {
            pm,
            proposer,
            scorer,
            seen: Mutex::new(HashSet::new()),
            tick: Duration::from_secs(3),
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
            // Skip if the proposer already consumed this task's signal. The
            // spur:signal-processed:<mutation_id> label is written by
            // mutation_executor::apply_mutation on commit.
            if issue
                .labels
                .iter()
                .any(|label| label.starts_with("spur:signal-processed:"))
            {
                continue;
            }

            let mut comments = adv.list_comments(&issue.id).await?;
            comments.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });

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
                if self.seen.lock().contains(&signal_id) {
                    continue;
                }

                let state = stub_plan_state();
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
                    Some((_score, batch)) => match apply_mutation(self.pm.clone(), &batch).await {
                        Ok(_) => {
                            self.seen.lock().insert(signal_id);
                        }
                        Err(error) => {
                            tracing::warn!(
                                issue_id = %issue.id,
                                %signal_id,
                                "signal watcher failed to apply mutation; will retry next tick: {error}"
                            );
                        }
                    },
                    None => {
                        self.seen.lock().insert(signal_id);
                    }
                }
            }
        }

        Ok(())
    }
}

fn stub_plan_state() -> PlanState {
    PlanState {
        plan_id: "signal-watcher-stub".into(),
        tasks: Vec::new(),
        brain_session_id: BrainSessionId::new(SessionId("signal-watcher".into())),
        base_snapshot_branch: None,
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: None,
    }
}
