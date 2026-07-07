use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use spur_pm::{IssueFilter, IssueSummary, IssueUpdate};

use crate::plan::audit_sentinel::{encode_comment, AuditSentinelKind};
use crate::plan::labels;
use crate::plan::loops::spec::{AutonomyLevel, FailureBackoff, LoopSpec};
use crate::plan::projector::collect_sorted_audits_for_issue;
use crate::plan::reconciler::Reconciler;

struct InvalidTemplatePause<'a> {
    summary: &'a IssueSummary,
    spec: &'a LoopSpec,
    generation: u32,
    now: i64,
    validation_error: String,
    advanced: &'a dyn spur_pm::BeadsAdvanced,
    dispatch: &'a dyn crate::plan::reconciler::ReconcilerDispatch,
}

struct RearmLoop<'a> {
    summary: &'a IssueSummary,
    loop_id: &'a str,
    generation: u32,
    now: i64,
    interval_secs: u64,
    add_labels: Vec<String>,
    event_sink: Option<&'a Arc<dyn spur_mcp::events::McpEventSink>>,
}

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
                issue_type: Some(super::LOOP_ISSUE_TYPE.to_string()),
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
                            Some(spec.autonomy.as_str().to_string()),
                            "budget_exhausted",
                            now,
                        )),
                    )
                    .await?;
                self.rearm_loop(RearmLoop {
                    summary: &summary,
                    loop_id: &spec.loop_id,
                    generation: next_generation,
                    now,
                    interval_secs,
                    add_labels: Vec::new(),
                    event_sink: dispatch.event_sink(),
                })
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
                            Some(spec.autonomy.as_str().to_string()),
                            "skipped_overlap",
                            now,
                        )),
                    )
                    .await?;
                self.rearm_loop(RearmLoop {
                    summary: &summary,
                    loop_id: &spec.loop_id,
                    generation: next_generation,
                    now,
                    interval_secs,
                    add_labels: Vec::new(),
                    event_sink: dispatch.event_sink(),
                })
                .await?;
                did_work = true;
                continue;
            }

            if spec.autonomy == AutonomyLevel::L3 {
                let mut input =
                    match loop_template_to_persist_input(&spec.template, &spec, next_generation) {
                        Ok(input) => input,
                        Err(error) => {
                            tracing::warn!(
                                loop_issue_id = %summary.id,
                                loop_id = %spec.loop_id,
                                generation = next_generation,
                                "loop scheduler auto-pausing invalid L3 template: {error}"
                            );
                            self.auto_pause_invalid_template_loop(InvalidTemplatePause {
                                summary: &summary,
                                spec: &spec,
                                generation: next_generation,
                                now,
                                validation_error: error.to_string(),
                                advanced,
                                dispatch: dispatch.as_ref(),
                            })
                            .await?;
                            did_work = true;
                            continue;
                        }
                    };
                input.brain_session_id = dispatch.brain_session_id().clone();
                if input.base.is_some() {
                    input.repo_root = Some(self.config.repo_root.clone());
                }
                input.event_sink = dispatch.event_sink().map(Arc::clone);
                input.reconciler_fast_forward = Some(Arc::clone(&self.fast_forward));
                crate::plan::persist_plan_as_epic(
                    self.pm.as_ref(),
                    self.feature_gate.as_ref(),
                    input,
                )
                .await?;
            } else {
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
            }
            self.rearm_loop(RearmLoop {
                summary: &summary,
                loop_id: &spec.loop_id,
                generation: next_generation,
                now,
                interval_secs,
                add_labels: Vec::new(),
                event_sink: dispatch.event_sink(),
            })
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
        if let Some(sink) = dispatch.event_sink() {
            sink.emit(spur_acp::SpurEventBody::LoopPaused {
                loop_id: spec.loop_id.clone(),
                by: "auto_paused".to_string(),
            });
        }
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

    async fn auto_pause_invalid_template_loop(
        &self,
        pause: InvalidTemplatePause<'_>,
    ) -> anyhow::Result<()> {
        self.pm
            .update_issue(
                &pause.summary.id,
                IssueUpdate {
                    add_labels: vec![labels::LOOP_PAUSED.to_string()],
                    remove_labels: pause
                        .summary
                        .labels
                        .iter()
                        .filter(|label| labels::parse_loop_next_run(label).is_some())
                        .cloned()
                        .collect(),
                    ..Default::default()
                },
            )
            .await?;
        if let Some(sink) = pause.dispatch.event_sink() {
            sink.emit(spur_acp::SpurEventBody::LoopPaused {
                loop_id: pause.spec.loop_id.clone(),
                by: "auto_paused".to_string(),
            });
        }
        pause
            .advanced
            .add_comment(
                &pause.summary.id,
                &encode_comment(&invalid_template_loop_run(
                    &pause.spec.loop_id,
                    pause.generation,
                    Some(pause.spec.autonomy.as_str().to_string()),
                    pause.now,
                )),
            )
            .await?;
        crate::plan::push_loop_template_validation_escalation_continuation(
            pause.dispatch.continuation_ctx().as_ref(),
            pause.dispatch.materializer().as_ref(),
            pause.dispatch.brain_session_id(),
            &pause.spec.loop_id,
            &pause.spec.goal,
            pause.generation,
            &pause.validation_error,
        )
        .await;
        Ok(())
    }

    async fn rearm_loop(&self, rearm: RearmLoop<'_>) -> anyhow::Result<()> {
        let interval = i64::try_from(rearm.interval_secs).unwrap_or(i64::MAX);
        let next_run = rearm.now.saturating_add(interval);
        let mut add = vec![labels::loop_next_run_label(next_run)];
        add.extend(rearm.add_labels);
        self.pm
            .update_issue(
                &rearm.summary.id,
                IssueUpdate {
                    add_labels: add,
                    remove_labels: rearm
                        .summary
                        .labels
                        .iter()
                        .filter(|label| labels::parse_loop_next_run(label).is_some())
                        .cloned()
                        .collect(),
                    ..Default::default()
                },
            )
            .await?;
        if let Some(sink) = rearm.event_sink {
            sink.emit(spur_acp::SpurEventBody::LoopArmed {
                loop_id: rearm.loop_id.to_string(),
                generation: rearm.generation,
                next_run,
            });
        }
        Ok(())
    }
}

pub(crate) fn loop_template_to_persist_input(
    template: &serde_json::Value,
    spec: &LoopSpec,
    generation: u32,
) -> anyhow::Result<crate::plan::PersistPlanAsEpicInput> {
    let template_object = template
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("loop template must be a JSON object"))?;
    let tasks_value = template_object
        .get("tasks")
        .ok_or_else(|| anyhow::anyhow!("loop template must include a tasks array"))?;
    let tasks_array = tasks_value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("loop template tasks must be an array"))?;
    let mut tasks = tasks_array
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<crate::plan::PlanTask>, _>>()
        .map_err(|error| anyhow::anyhow!("loop template task format is invalid: {error}"))?;
    let auto_serialized =
        crate::plan::submit_plan_normalize_tasks(&mut tasks).map_err(anyhow::Error::msg)?;
    let epic_title = template_object
        .get("epic_title")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(String::from)
        .or_else(|| {
            tasks
                .first()
                .map(|task| task.task.trim().chars().take(60).collect::<String>())
                .filter(|title| !title.is_empty())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loop template cannot derive epic_title - provide epic_title or a non-empty first task body"
            )
        })?;
    let epic_body = template_object
        .get("epic_body")
        .and_then(|value| value.as_str())
        .map(String::from);
    let base = match template_object.get("base") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<crate::BaseTarget>(value.clone())
                .map_err(|error| anyhow::anyhow!("loop template base is invalid: {error}"))?,
        ),
    };
    let mut epic_labels = vec![
        labels::loop_id_label(&spec.loop_id),
        labels::loop_generation_label(generation),
        format!("{}{}", labels::AUTONOMY_PREFIX, spec.autonomy.as_str()),
    ];
    if let Some(max_cost) = spec.governors.max_cost_micros_per_generation {
        epic_labels.push(format!("{}{}", labels::LOOP_BUDGET_MICROS_PREFIX, max_cost));
    }

    Ok(crate::plan::PersistPlanAsEpicInput {
        tasks,
        base,
        parent_epic_id: None,
        epic_title: Some(epic_title),
        epic_body,
        epic_labels,
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(format!(
            "loop-{}",
            spec.loop_id
        ))),
        execution_mode: "loop_l3_generation".to_string(),
        precomputed_auto_serialized: Some(auto_serialized),
        repo_root: None,
        active_plans: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        event_sink: None,
        reconciler_fast_forward: None,
    })
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

fn skipped_loop_run(
    loop_id: &str,
    generation: u32,
    autonomy: Option<String>,
    outcome: &str,
    now: i64,
) -> AuditSentinelKind {
    AuditSentinelKind::LoopRun {
        loop_id: loop_id.to_string(),
        generation,
        plan_id: String::new(),
        autonomy,
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

fn invalid_template_loop_run(
    loop_id: &str,
    generation: u32,
    autonomy: Option<String>,
    now: i64,
) -> AuditSentinelKind {
    AuditSentinelKind::LoopRun {
        loop_id: loop_id.to_string(),
        generation,
        plan_id: String::new(),
        autonomy,
        outcome: "invalid_template".to_string(),
        tasks_discovered: 0,
        approved: 0,
        rejected: 0,
        failed: 0,
        cancelled: 0,
        escalations: 1,
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
