//! Level-triggered reconciler for beads-backed plans.
//!
//! Ticks on an adaptive cadence: fast when there is activity, backing off
//! toward an idle ceiling when there is not. In v0a.2 the reconciler only
//! observes/parity-checks beads state; it does NOT dispatch ACP work.
//!
//! Primary engine: `bv --robot-triage` via BvAdapter (see plan
//! addendum II in docs/superpowers/plans/2026-04-20-adaptive-plan-repair-v0a.md
//! for the rationale — upstream AGENTS.md designates bv as the canonical
//! pick-next-work surface). Fallback: `br ready` via BeadsAdvanced when bv
//! errors. The bv primary path enforces the `spur:plan-complete` guard; the br
//! fallback uses `spur:plan-id:<id>` scoping only (degraded-mode semantics —
//! see `observe_ready_via_br` doc comment).
//!
//! # Spawn wiring
//!
//! TODO(v0b): wire `Reconciler::run` into `server.rs` startup. In v0a.2 the
//! reconciler is created and tested in isolation only — no server integration.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use spur_pm::{PmService, ReadyFilter};

pub struct ReconcilerConfig {
    pub base_interval: Duration,
    pub idle_ceiling: Duration,
    pub backoff_factor: u32,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(3),
            idle_ceiling: Duration::from_secs(30),
            backoff_factor: 2,
        }
    }
}

pub struct Reconciler {
    config: ReconcilerConfig,
    pm: Arc<PmService>,
    fast_forward: Arc<Notify>,
    plan_id: Option<String>,
}

impl Reconciler {
    pub fn new(
        config: ReconcilerConfig,
        pm: Arc<PmService>,
        fast_forward: Arc<Notify>,
        plan_id: Option<String>,
    ) -> Self {
        Self {
            config,
            pm,
            fast_forward,
            plan_id,
        }
    }

    pub async fn run(self, cancel: tokio::sync::oneshot::Receiver<()>) {
        let mut interval = self.config.base_interval;
        tokio::pin!(cancel);
        loop {
            tokio::select! {
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel");
                    break;
                }
                _ = self.fast_forward.notified() => {
                    tracing::debug!("reconciler fast-forward triggered");
                    interval = self.config.base_interval;
                }
                _ = tokio::time::sleep(interval) => {}
            }
            // Race tick_once against cancel so shutdown cannot hang behind
            // stuck PM I/O (bv.triage / br ready). Aborting a tick's partial
            // I/O is acceptable: the reconciler is observation-only in v0a.2
            // and performs no state mutation to roll back.
            let did_work = tokio::select! {
                biased;
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel during tick");
                    break;
                }
                result = self.tick_once() => {
                    match result {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("reconciler tick failed: {e}");
                            false
                        }
                    }
                }
            };
            if did_work {
                interval = self.config.base_interval;
            } else {
                let scaled = interval.saturating_mul(self.config.backoff_factor);
                interval = std::cmp::min(scaled, self.config.idle_ceiling);
            }
        }
    }

    async fn tick_once(&self) -> anyhow::Result<bool> {
        let ready_ids = self.observe_ready().await?;
        for id in &ready_ids {
            tracing::debug!(%id, "reconciler observed ready task");
        }
        Ok(!ready_ids.is_empty())
    }

    /// Returns the IDs of ready tasks under the configured plan filter,
    /// using bv primary + br fallback.
    pub async fn observe_ready(&self) -> anyhow::Result<Vec<String>> {
        let label_filter = self.plan_id.as_deref().map(crate::plan::labels::plan_id);

        // Primary: bv triage. Use `quick_ref.top_picks` — bv's curated list of
        // actionable (unblocked) issues — rather than the broader
        // `recommendations` list which includes blocked issues as well.
        if let Some(bv) = self.pm.analyzer() {
            match bv.triage(label_filter.as_deref()).await {
                Ok(report) => {
                    return Ok(report
                        .triage
                        .quick_ref
                        .top_picks
                        .into_iter()
                        .map(|p| p.id)
                        .collect());
                }
                Err(e) => {
                    tracing::warn!("bv triage failed, falling back to br ready: {e}");
                }
            }
        }

        // Fallback: br ready via observe_ready_via_br.
        self.observe_ready_via_br().await
    }

    /// Fallback ready-task query using `br ready` (BeadsAdvanced) directly.
    ///
    /// # Degraded-mode semantics
    ///
    /// `spur:plan-complete` is an epic-only marker — tasks never carry it, so
    /// including it in a `ReadyFilter` (which queries tasks, not epics) always
    /// returns empty. The `bv` primary path is the real guard against observing
    /// partially-persisted plan graphs; this fallback scopes only by
    /// `spur:plan-id:<id>` (when `plan_id` is `Some`), accepting that a partial
    /// plan could leak through in the rare window where bv is unhealthy and the
    /// caller passed a plan_id that was not fully persisted. This tradeoff is
    /// acceptable for v0a.2: fallback only triggers on bv failures, and callers
    /// are expected to target fully-persisted plans.
    ///
    /// "Observe all plans" mode (`plan_id` is `None`): no label filter is
    /// applied; all unblocked tasks are returned. Partial-plan protection is
    /// entirely absent in this mode — document as v0a.2 limitation.
    pub async fn observe_ready_via_br(&self) -> anyhow::Result<Vec<String>> {
        let label_filter = self.plan_id.as_deref().map(crate::plan::labels::plan_id);

        let Some(adv) = self.pm.advanced() else {
            anyhow::bail!("reconciler: no advanced (beads) backend available");
        };
        // Fallback is degraded-mode observation. The PLAN_COMPLETE gate is
        // enforced in the bv primary path; here we rely on the caller's plan_id
        // scoping. Partial plans may leak through during fallback — acceptable
        // tradeoff for v0a.2 since fallback only triggers on bv failures (rare).
        let mut labels = Vec::new();
        if let Some(pid_label) = label_filter {
            labels.push(pid_label);
        }
        let summaries = adv
            .list_ready(ReadyFilter {
                labels_all: labels,
                limit: Some(50),
                ..Default::default()
            })
            .await?;
        Ok(summaries.into_iter().map(|s| s.id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D1 fix coverage: verify that the biased select! pattern used inside
    /// `Reconciler::run` to race `tick_once` against `cancel` actually
    /// preempts an in-flight future when cancel fires. Uses a pending future
    /// as a stand-in for a stuck `bv.triage`/`br ready` call; without the
    /// biased cancel race, the task would hang indefinitely.
    #[tokio::test]
    async fn biased_select_cancel_preempts_pending_tick() {
        use std::future::pending;
        use tokio::sync::oneshot;

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        tokio::pin!(cancel_rx);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = cancel_tx.send(());
        });

        let blocking = pending::<anyhow::Result<bool>>();
        tokio::pin!(blocking);

        let outcome = tokio::time::timeout(Duration::from_secs(1), async move {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => "cancelled",
                _ = &mut blocking => "tick_completed",
            }
        })
        .await
        .expect("select must not hang when cancel is live");

        assert_eq!(outcome, "cancelled");
    }

    #[test]
    fn cadence_backoff_formula() {
        let cfg = ReconcilerConfig {
            base_interval: Duration::from_secs(1),
            idle_ceiling: Duration::from_secs(8),
            backoff_factor: 2,
        };
        let mut d = cfg.base_interval;
        let mut hist = vec![d];
        for _ in 0..5 {
            d = std::cmp::min(d.saturating_mul(cfg.backoff_factor), cfg.idle_ceiling);
            hist.push(d);
        }
        assert_eq!(
            hist,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }
}
