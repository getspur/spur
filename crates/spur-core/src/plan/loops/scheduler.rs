use std::time::{SystemTime, UNIX_EPOCH};

use spur_pm::{IssueFilter, IssueSummary, IssueUpdate};

use crate::plan::audit_sentinel::{encode_comment, AuditSentinelKind};
use crate::plan::labels;
use crate::plan::loops::spec::{FailureBackoff, LoopSpec};
use crate::plan::projector::collect_sorted_audits_for_issue;
use crate::plan::reconciler::Reconciler;

impl Reconciler {
    /// One pass over open loop issues. Returns true if any loop was armed or a
    /// durable loop record/control update was written.
    pub(crate) async fn run_loop_scheduler_sweep(&self) -> anyhow::Result<bool> {
        if !self.config.loops_enabled {
            return Ok(false);
        }
        let Some(dispatch) = self.dispatch.as_ref() else {
            return Ok(false);
        };
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let Some(advanced) = self.pm.advanced() else {
            return Ok(false);
        };
        if self.global_loop_pause_active().await? {
            return Ok(false);
        }

        let now = system_time_to_unix_seconds(self.clock.now());
        let loop_summaries = self
            .pm
            .list_issues(IssueFilter {
                status: Some("open".to_string()),
                issue_type: Some("task".to_string()),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let mut did_work = false;

        for summary in loop_summaries
            .into_iter()
            .filter(|summary| has_loop_id_label(&summary.labels))
        {
            if summary
                .labels
                .iter()
                .any(|label| label == labels::LOOP_PAUSED)
            {
                continue;
            }
            let issue = self.pm.get_issue(&summary.id).await?;
            let spec = match LoopSpec::parse(&issue.body) {
                Ok(spec) => spec,
                Err(error) => {
                    tracing::warn!(
                        loop_issue_id = %summary.id,
                        "loop scheduler skipped unparseable LoopSpec: {error}"
                    );
                    continue;
                }
            };
            if next_run_after_now(&summary.labels, now) {
                continue;
            }

            let audits = collect_sorted_audits_for_issue(
                &summary.id,
                advanced.list_comments(&summary.id).await?,
            )?;
            let next_generation = self.next_loop_generation(&spec.loop_id).await?;
            let consecutive_failures = trailing_failed_loop_runs(&audits, &spec.loop_id);
            if self
                .auto_pause_failed_loop(&summary, &spec, consecutive_failures, dispatch.as_ref())
                .await?
            {
                did_work = true;
                continue;
            }

            let interval_secs = effective_interval_secs(&spec, consecutive_failures);
            if generations_in_last_day(&audits, &spec.loop_id, now)
                >= spec.governors.max_generations_per_day.unwrap_or(u32::MAX)
            {
                advanced
                    .add_comment(
                        &summary.id,
                        &encode_comment(&skipped_loop_run(
                            &spec.loop_id,
                            next_generation,
                            "budget_exhausted",
                            now,
                        )),
                    )
                    .await?;
                self.rearm_loop(&summary, now, interval_secs, Vec::new())
                    .await?;
                did_work = true;
                continue;
            }

            if self.live_generation_exists(&spec.loop_id).await? {
                advanced
                    .add_comment(
                        &summary.id,
                        &encode_comment(&skipped_loop_run(
                            &spec.loop_id,
                            next_generation,
                            "skipped_overlap",
                            now,
                        )),
                    )
                    .await?;
                self.rearm_loop(&summary, now, interval_secs, Vec::new())
                    .await?;
                did_work = true;
                continue;
            }

            crate::plan::push_loop_due_continuation(
                dispatch.continuation_ctx().as_ref(),
                dispatch.materializer().as_ref(),
                dispatch.brain_session_id(),
                &spec.loop_id,
                &spec.goal,
                next_generation,
                &spec.template,
            )
            .await;
            self.rearm_loop(&summary, now, interval_secs, Vec::new())
                .await?;
            did_work = true;
        }

        Ok(did_work)
    }

    async fn global_loop_pause_active(&self) -> anyhow::Result<bool> {
        if self.config.pause_all_loops {
            return Ok(true);
        }
        Ok(!self
            .pm
            .list_issues(IssueFilter {
                labels: vec![labels::PAUSE_ALL_LOOPS.to_string()],
                status: Some("open".to_string()),
                limit: Some(1),
                ..Default::default()
            })
            .await?
            .is_empty())
    }

    async fn next_loop_generation(&self, loop_id: &str) -> anyhow::Result<u32> {
        let max_seen = self
            .pm
            .list_issues(IssueFilter {
                labels: vec![labels::loop_id_label(loop_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10_000),
                ..Default::default()
            })
            .await?
            .iter()
            .filter_map(|summary| {
                summary
                    .labels
                    .iter()
                    .find_map(|label| labels::parse_loop_generation(label))
            })
            .max()
            .unwrap_or(0);
        Ok(max_seen.saturating_add(1))
    }

    async fn live_generation_exists(&self, loop_id: &str) -> anyhow::Result<bool> {
        Ok(!self
            .pm
            .list_issues(IssueFilter {
                labels: vec![labels::loop_id_label(loop_id)],
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                limit: Some(1),
                ..Default::default()
            })
            .await?
            .is_empty())
    }

    async fn auto_pause_failed_loop(
        &self,
        summary: &IssueSummary,
        spec: &LoopSpec,
        consecutive_failures: u32,
        dispatch: &dyn crate::plan::reconciler::ReconcilerDispatch,
    ) -> anyhow::Result<bool> {
        let Some(backoff) = spec.governors.consecutive_failure_backoff.as_ref() else {
            return Ok(false);
        };
        if backoff.auto_pause_after == 0 || consecutive_failures < backoff.auto_pause_after {
            return Ok(false);
        }
        self.pm
            .update_issue(
                &summary.id,
                IssueUpdate {
                    add_labels: vec![labels::LOOP_PAUSED.to_string()],
                    remove_labels: summary
                        .labels
                        .iter()
                        .filter(|label| labels::parse_loop_next_run(label).is_some())
                        .cloned()
                        .collect(),
                    ..Default::default()
                },
            )
            .await?;
        crate::plan::push_loop_escalation_continuation(
            dispatch.continuation_ctx().as_ref(),
            dispatch.materializer().as_ref(),
            dispatch.brain_session_id(),
            &spec.loop_id,
            &spec.goal,
            consecutive_failures,
        )
        .await;
        Ok(true)
    }

    async fn rearm_loop(
        &self,
        summary: &IssueSummary,
        now: i64,
        interval_secs: u64,
        add_labels: Vec<String>,
    ) -> anyhow::Result<()> {
        let interval = i64::try_from(interval_secs).unwrap_or(i64::MAX);
        let next_run = now.saturating_add(interval);
        let mut add = vec![labels::loop_next_run_label(next_run)];
        add.extend(add_labels);
        self.pm
            .update_issue(
                &summary.id,
                IssueUpdate {
                    add_labels: add,
                    remove_labels: summary
                        .labels
                        .iter()
                        .filter(|label| labels::parse_loop_next_run(label).is_some())
                        .cloned()
                        .collect(),
                    ..Default::default()
                },
            )
            .await
    }
}

fn has_loop_id_label(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|label| crate::plan::labels::parse_loop_id(label).is_some())
}

fn next_run_after_now(labels: &[String], now: i64) -> bool {
    labels
        .iter()
        .filter_map(|label| labels::parse_loop_next_run(label))
        .max()
        .is_some_and(|next_run| next_run > now)
}

fn skipped_loop_run(loop_id: &str, generation: u32, outcome: &str, now: i64) -> AuditSentinelKind {
    AuditSentinelKind::LoopRun {
        loop_id: loop_id.to_string(),
        generation,
        plan_id: String::new(),
        outcome: outcome.to_string(),
        tasks_discovered: 0,
        approved: 0,
        rejected: 0,
        failed: 0,
        cancelled: 0,
        escalations: 0,
        cost_micros: 0,
        started_at: now,
        ended_at: now,
    }
}

pub(crate) fn trailing_failed_loop_runs(audits: &[AuditSentinelKind], loop_id: &str) -> u32 {
    let mut count = 0u32;
    for audit in audits.iter().rev() {
        let AuditSentinelKind::LoopRun {
            loop_id: record_loop_id,
            outcome,
            ..
        } = audit
        else {
            continue;
        };
        if record_loop_id != loop_id {
            continue;
        }
        if outcome == "failed" {
            count = count.saturating_add(1);
        } else {
            break;
        }
    }
    count
}

fn generations_in_last_day(audits: &[AuditSentinelKind], loop_id: &str, now: i64) -> u32 {
    let since = now.saturating_sub(86_400);
    audits
        .iter()
        .filter(|audit| {
            matches!(
                audit,
                AuditSentinelKind::LoopRun {
                    loop_id: record_loop_id,
                    outcome,
                    ended_at,
                    ..
                } if record_loop_id == loop_id
                    && matches!(outcome.as_str(), "approved" | "partial" | "failed")
                    && *ended_at >= since
            )
        })
        .count()
        .min(u32::MAX as usize) as u32
}

pub(crate) fn effective_interval_secs(spec: &LoopSpec, consecutive_failures: u32) -> u64 {
    let Some(FailureBackoff {
        k,
        factor,
        auto_pause_after,
    }) = spec.governors.consecutive_failure_backoff
    else {
        return spec.cadence_secs;
    };
    if consecutive_failures == 0 || k == 0 || factor <= 1 {
        return spec.cadence_secs;
    }
    let max_exponent = if auto_pause_after == 0 {
        consecutive_failures / k
    } else {
        auto_pause_after.saturating_add(k.saturating_sub(1)) / k
    };
    let exponent = (consecutive_failures / k).min(max_exponent);
    spec.cadence_secs
        .saturating_mul(u64::from(factor).saturating_pow(exponent))
}

fn system_time_to_unix_seconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
