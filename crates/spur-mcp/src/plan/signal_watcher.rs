//! Brain-side signal watcher: polls `awaiting_review` tasks bearing
//! `signal:*` labels, dedupes by `signal_id`, invokes a `MutationProposer` +
//! `MutationScorer`, and applies the highest-scored batch.

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

        for issue in candidates {
            if issue.status != "awaiting_review" {
                continue;
            }
            if !issue
                .labels
                .iter()
                .any(|label| label.starts_with("signal:"))
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
                if !self.seen.lock().insert(signal_id) {
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

                if let Some((_score, batch)) = scored_batches.into_iter().next() {
                    if let Err(error) = apply_mutation(self.pm.clone(), &batch).await {
                        tracing::warn!(
                            issue_id = %issue.id,
                            %signal_id,
                            "signal watcher failed to apply mutation: {error}"
                        );
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
        epic_id: None,
    }
}
