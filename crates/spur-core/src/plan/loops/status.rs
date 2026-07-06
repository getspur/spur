#![allow(dead_code)]

use anyhow::{anyhow, Context};

use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::labels;
use crate::plan::loops::scheduler::{effective_interval_secs, trailing_failed_loop_runs};
use crate::plan::loops::spec::LoopSpec;
use crate::plan::projector::collect_sorted_audits_for_issue;

const LOOP_SUMMARY_LIST_LIMIT: usize = 1000;
const LOOP_SUMMARY_EMIT_LIMIT: usize = 200;
const LOOP_DETAIL_EVENT_RECENT_LIMIT: usize = 20;

#[derive(Debug, Clone)]
pub(crate) struct LoopStatus {
    pub loop_id: String,
    pub issue_id: String,
    pub title: String,
    pub spec: LoopSpec,
    pub recent_runs: Vec<crate::plan::audit_sentinel::AuditSentinelKind>,
    pub consecutive_failures: u32,
    pub effective_interval_secs: u64,
    pub backoff_active: bool,
    pub paused: bool,
    pub next_run: Option<i64>,
}

impl LoopStatus {
    pub(crate) fn to_mcp_json(&self) -> serde_json::Value {
        let recent_runs: Vec<serde_json::Value> = self
            .recent_runs
            .iter()
            .filter_map(|audit| serde_json::to_value(audit).ok())
            .collect();
        serde_json::json!({
            "loop_id": &self.loop_id,
            "issue_id": &self.issue_id,
            "spec": &self.spec,
            "recent_runs": recent_runs,
            "consecutive_failures": self.consecutive_failures,
            "effective_interval_secs": self.effective_interval_secs,
            "backoff_active": self.backoff_active,
            "paused": self.paused,
            "next_run": self.next_run,
        })
    }

    pub(crate) fn to_detail_event(&self) -> spur_acp::LoopDetailEvent {
        let start = self
            .recent_runs
            .len()
            .saturating_sub(LOOP_DETAIL_EVENT_RECENT_LIMIT);
        spur_acp::LoopDetailEvent {
            loop_id: self.loop_id.clone(),
            issue_id: self.issue_id.clone(),
            title: self.title.clone(),
            goal_preview: text_preview(&self.spec.goal),
            cadence_secs: self.spec.cadence_secs,
            effective_interval_secs: self.effective_interval_secs,
            backoff_active: self.backoff_active,
            paused: self.paused,
            next_run: self.next_run,
            consecutive_failures: self.consecutive_failures,
            budget_micros_per_generation: self.spec.governors.max_cost_micros_per_generation,
            max_generations_per_day: self.spec.governors.max_generations_per_day,
            max_tasks: self.spec.governors.max_tasks_per_generation,
            recent_runs: self.recent_runs[start..]
                .iter()
                .filter_map(loop_run_record_event)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopSummaryLoad {
    pub loops: Vec<spur_acp::LoopSummaryEvent>,
    pub warnings: Vec<String>,
}

pub(crate) async fn build_loop_status(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
    recent_limit: usize,
) -> anyhow::Result<Option<LoopStatus>> {
    let Some(issue) = load_loop_issue(pm, loop_id).await? else {
        return Ok(None);
    };
    let spec = LoopSpec::parse(&issue.body)
        .map_err(|error| anyhow!("failed to parse loop spec: {error}"))?;
    let advanced = pm
        .advanced()
        .ok_or_else(|| anyhow!("beads advanced comments API is unavailable"))?;
    let comments = advanced
        .list_comments(&issue.id)
        .await
        .map_err(|error| anyhow!("failed to list loop comments: {error}"))?;
    let audits = collect_sorted_audits_for_issue(&issue.id, comments)
        .map_err(|error| anyhow!("failed to parse loop audits: {error}"))?;

    Ok(Some(loop_status_from_issue(
        issue,
        spec,
        audits,
        loop_id,
        recent_limit,
    )))
}

pub(crate) async fn load_loop_summaries(
    pm: &dyn crate::plan::PmLike,
) -> anyhow::Result<LoopSummaryLoad> {
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            issue_type: Some("task".to_string()),
            include_closed: true,
            limit: Some(LOOP_SUMMARY_LIST_LIMIT),
            ..Default::default()
        })
        .await?;

    let mut load = LoopSummaryLoad::default();
    let advanced = pm.advanced();
    if advanced.is_none() {
        load.warnings.push(
            "loop summaries: beads advanced comments API is unavailable; run fields degraded"
                .to_string(),
        );
    }

    for summary in summaries {
        let Some(loop_id) = loop_id_from_labels(&summary.labels) else {
            continue;
        };
        let body = summary.description.as_deref().unwrap_or_default();
        let spec = match LoopSpec::parse(body) {
            Ok(spec) => spec,
            Err(error) => {
                load.warnings.push(format!(
                    "skipping loop issue {} ({loop_id}): failed to parse LoopSpec: {error}",
                    summary.id
                ));
                continue;
            }
        };

        let metrics = match advanced {
            Some(advanced) => match load_loop_audits(advanced, &summary.id).await {
                Ok(audits) => LoopAuditMetrics::from_audits(&spec, &loop_id, audits),
                Err(error) => {
                    tracing::warn!(
                        loop_id = %loop_id,
                        issue_id = %summary.id,
                        error = %error,
                        "failed to load loop summary audits"
                    );
                    load.warnings.push(format!(
                        "loop issue {} ({loop_id}): failed to load loop comments: {error}",
                        summary.id
                    ));
                    LoopAuditMetrics::degraded(&spec)
                }
            },
            None => LoopAuditMetrics::degraded(&spec),
        };

        load.loops.push(spur_acp::LoopSummaryEvent {
            loop_id,
            issue_id: summary.id,
            title: summary.title,
            autonomy: autonomy_from_labels(&summary.labels),
            paused: summary
                .labels
                .iter()
                .any(|label| label == labels::LOOP_PAUSED),
            retired: summary.status == pm.closed_status(),
            backoff_active: metrics.effective_interval_secs > spec.cadence_secs,
            cadence_secs: spec.cadence_secs,
            effective_interval_secs: metrics.effective_interval_secs,
            next_run: next_run_from_labels(&summary.labels),
            last_generation: metrics.last_generation,
            last_outcome: metrics.last_outcome,
            last_cost_micros: metrics.last_cost_micros,
            consecutive_failures: metrics.consecutive_failures,
            goal_preview: text_preview(&spec.goal),
            updated_at: None,
        });
    }

    if load.loops.len() > LOOP_SUMMARY_EMIT_LIMIT {
        let original_len = load.loops.len();
        load.loops.truncate(LOOP_SUMMARY_EMIT_LIMIT);
        load.warnings.push(format!(
            "loop summaries truncated from {original_len} to {LOOP_SUMMARY_EMIT_LIMIT} rows"
        ));
    }

    Ok(load)
}

async fn load_loop_issue(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
) -> anyhow::Result<Option<spur_pm::Issue>> {
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![labels::loop_id_label(loop_id)],
            issue_type: Some("task".to_string()),
            limit: Some(2),
            ..Default::default()
        })
        .await
        .with_context(|| "failed to load loop issue")?;
    let Some(summary) = summaries.first() else {
        return Ok(None);
    };
    pm.get_issue(&summary.id)
        .await
        .map(Some)
        .with_context(|| "failed to load loop issue")
}

async fn load_loop_audits(
    advanced: &dyn spur_pm::BeadsAdvanced,
    issue_id: &str,
) -> anyhow::Result<Vec<AuditSentinelKind>> {
    let comments = advanced.list_comments(issue_id).await?;
    collect_sorted_audits_for_issue(issue_id, comments)
        .map_err(|error| anyhow!("failed to parse loop audits: {error}"))
}

fn loop_status_from_issue(
    issue: spur_pm::Issue,
    spec: LoopSpec,
    audits: Vec<AuditSentinelKind>,
    loop_id: &str,
    recent_limit: usize,
) -> LoopStatus {
    let metrics = LoopAuditMetrics::from_audits(&spec, loop_id, audits.clone());
    let recent_runs = recent_loop_runs(&audits, loop_id, recent_limit);
    let paused = issue
        .labels
        .iter()
        .any(|label| label == labels::LOOP_PAUSED);
    let next_run = next_run_from_labels(&issue.labels);

    LoopStatus {
        loop_id: loop_id.to_string(),
        issue_id: issue.id,
        title: issue.title,
        spec,
        recent_runs,
        consecutive_failures: metrics.consecutive_failures,
        effective_interval_secs: metrics.effective_interval_secs,
        backoff_active: metrics.effective_interval_secs > metrics.cadence_secs,
        paused,
        next_run,
    }
}

#[derive(Debug, Clone)]
struct LoopAuditMetrics {
    cadence_secs: u64,
    effective_interval_secs: u64,
    consecutive_failures: u32,
    last_generation: Option<u32>,
    last_outcome: Option<String>,
    last_cost_micros: Option<u64>,
}

impl LoopAuditMetrics {
    fn degraded(spec: &LoopSpec) -> Self {
        Self {
            cadence_secs: spec.cadence_secs,
            effective_interval_secs: spec.cadence_secs,
            consecutive_failures: 0,
            last_generation: None,
            last_outcome: None,
            last_cost_micros: None,
        }
    }

    fn from_audits(spec: &LoopSpec, loop_id: &str, audits: Vec<AuditSentinelKind>) -> Self {
        let consecutive_failures = trailing_failed_loop_runs(&audits, loop_id);
        let effective_interval_secs = effective_interval_secs(spec, consecutive_failures);
        let mut metrics = Self {
            cadence_secs: spec.cadence_secs,
            effective_interval_secs,
            consecutive_failures,
            last_generation: None,
            last_outcome: None,
            last_cost_micros: None,
        };

        if let Some(AuditSentinelKind::LoopRun {
            generation,
            outcome,
            cost_micros,
            ..
        }) = audits.iter().rev().find(|audit| {
            matches!(
                audit,
                AuditSentinelKind::LoopRun {
                    loop_id: record_loop_id,
                    ..
                } if record_loop_id == loop_id
            )
        }) {
            metrics.last_generation = Some(*generation);
            metrics.last_outcome = Some(outcome.clone());
            metrics.last_cost_micros = Some(*cost_micros);
        }

        metrics
    }
}

fn recent_loop_runs(
    audits: &[AuditSentinelKind],
    loop_id: &str,
    recent_limit: usize,
) -> Vec<AuditSentinelKind> {
    let mut recent_runs: Vec<AuditSentinelKind> = audits
        .iter()
        .rev()
        .filter_map(|audit| match audit {
            AuditSentinelKind::LoopRun {
                loop_id: record_loop_id,
                ..
            } if record_loop_id == loop_id => Some(audit.clone()),
            _ => None,
        })
        .take(recent_limit)
        .collect();
    recent_runs.reverse();
    recent_runs
}

fn loop_run_record_event(audit: &AuditSentinelKind) -> Option<spur_acp::LoopRunRecordEvent> {
    match audit {
        AuditSentinelKind::LoopRun {
            generation,
            outcome,
            cost_micros,
            autonomy,
            ..
        } => Some(spur_acp::LoopRunRecordEvent {
            generation: *generation,
            outcome: outcome.clone(),
            cost_micros: *cost_micros,
            autonomy: autonomy.clone(),
        }),
        _ => None,
    }
}

fn loop_id_from_labels(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find_map(|label| labels::parse_loop_id(label).map(str::to_string))
}

fn autonomy_from_labels(labels: &[String]) -> Option<String> {
    labels.iter().find_map(|label| {
        label
            .strip_prefix(labels::AUTONOMY_PREFIX)
            .map(str::to_string)
    })
}

fn next_run_from_labels(labels: &[String]) -> Option<i64> {
    labels
        .iter()
        .filter_map(|label| labels::parse_loop_next_run(label))
        .max()
}

fn text_preview(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let mut preview: String = body.chars().take(500).collect();
    if body.chars().count() > 500 {
        preview.push_str("...");
    }
    Some(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    use crate::plan::loops::spec::{AutonomyLevel, FailureBackoff, LoopGovernors};
    use crate::plan::{labels, PmLike};

    fn test_spec(loop_id: &str, goal: &str) -> LoopSpec {
        LoopSpec {
            loop_id: loop_id.to_string(),
            goal: goal.to_string(),
            pattern: Some("ci-sweeper".to_string()),
            cadence_secs: 60,
            autonomy: AutonomyLevel::L1,
            template: json!({
                "tasks": [{
                    "task_id": "triage",
                    "agent": "codex",
                    "task": "Triage the loop",
                    "labels": [labels::LOOP_TRIAGE_TASK]
                }]
            }),
            governors: LoopGovernors {
                max_cost_micros_per_generation: Some(2_000_000),
                max_generations_per_day: Some(24),
                max_tasks_per_generation: Some(5),
                denylist_globs: Vec::new(),
                consecutive_failure_backoff: Some(FailureBackoff {
                    k: 2,
                    factor: 2,
                    auto_pause_after: 4,
                }),
            },
            escalation: None,
        }
    }

    async fn create_loop_issue(
        pm: &dyn PmLike,
        loop_id: &str,
        spec: LoopSpec,
        mut labels: Vec<String>,
    ) -> String {
        labels.push(labels::loop_id_label(loop_id));
        labels.sort();
        pm.create_issue(spur_pm::IssueCreate {
            title: format!("Loop: {}", spec.goal),
            description: Some(spec.to_sentinel_body()),
            labels,
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .expect("create loop issue")
    }

    async fn add_loop_run(
        pm: &crate::plan::test_util::MockPm,
        issue_id: &str,
        loop_id: &str,
        generation: u32,
        outcome: &str,
        cost_micros: u64,
    ) {
        let body = crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::LoopRun {
                loop_id: loop_id.to_string(),
                generation,
                plan_id: format!("plan-{generation}"),
                autonomy: Some("l1".to_string()),
                outcome: outcome.to_string(),
                tasks_discovered: generation,
                approved: u32::from(outcome == "approved"),
                rejected: 0,
                failed: u32::from(outcome == "failed"),
                cancelled: 0,
                escalations: 0,
                cost_micros,
                started_at: i64::from(generation),
                ended_at: i64::from(generation),
            },
        );
        spur_pm::BeadsAdvanced::add_comment(pm, issue_id, &body)
            .await
            .expect("add loop run");
    }

    #[derive(Clone)]
    struct NoAdvancedPm {
        inner: crate::plan::test_util::MockPm,
    }

    #[async_trait]
    impl PmLike for NoAdvancedPm {
        async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
            self.inner.get_issue(id).await
        }

        async fn list_issues(
            &self,
            filter: spur_pm::IssueFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            self.inner.list_issues(filter).await
        }

        async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
            self.inner.create_issue(params).await
        }

        async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
            self.inner.update_issue(id, update).await
        }

        fn closed_status(&self) -> &str {
            self.inner.closed_status()
        }
    }

    #[derive(Clone)]
    struct RecordingPm {
        inner: crate::plan::test_util::MockPm,
        filters: Arc<Mutex<Vec<spur_pm::IssueFilter>>>,
    }

    impl RecordingPm {
        fn new() -> Self {
            Self {
                inner: crate::plan::test_util::MockPm::new(),
                filters: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_filters(&self) -> Vec<spur_pm::IssueFilter> {
            self.filters.lock().expect("recorded filters lock").clone()
        }
    }

    #[async_trait]
    impl PmLike for RecordingPm {
        async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
            self.inner.get_issue(id).await
        }

        async fn list_issues(
            &self,
            filter: spur_pm::IssueFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            self.filters
                .lock()
                .expect("recorded filters lock")
                .push(filter.clone());
            self.inner.list_issues(filter).await
        }

        async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
            self.inner.create_issue(params).await
        }

        async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
            self.inner.update_issue(id, update).await
        }

        fn closed_status(&self) -> &str {
            self.inner.closed_status()
        }
    }

    #[tokio::test]
    async fn load_loop_summaries_requests_loop_issue_type() {
        let pm = RecordingPm::new();

        let load = load_loop_summaries(&pm).await.expect("load summaries");

        assert!(load.loops.is_empty());
        let filters = pm.recorded_filters();
        assert_eq!(filters.len(), 1);
        let filter = &filters[0];
        assert_eq!(filter.issue_type.as_deref(), Some("loop"));
        assert!(filter.include_closed);
        assert_eq!(filter.limit, Some(LOOP_SUMMARY_LIST_LIMIT));
    }

    #[tokio::test]
    async fn load_loop_issue_requests_loop_issue_type() {
        let pm = RecordingPm::new();
        let loop_id = "missingloop";

        let issue = load_loop_issue(&pm, loop_id)
            .await
            .expect("load loop issue");

        assert!(issue.is_none());
        let filters = pm.recorded_filters();
        assert_eq!(filters.len(), 1);
        let filter = &filters[0];
        assert_eq!(filter.labels, vec![labels::loop_id_label(loop_id)]);
        assert_eq!(filter.issue_type.as_deref(), Some("loop"));
        assert_eq!(filter.limit, Some(2));
        assert!(!filter.include_closed);
    }

    #[tokio::test]
    async fn load_loop_summaries_skips_mislabeled_task_with_warning() {
        let pm = crate::plan::test_util::MockPm::new();
        pm.create_issue(spur_pm::IssueCreate {
            title: "Not a real loop".to_string(),
            description: Some("plain task body".to_string()),
            labels: vec![labels::loop_id_label("bad-loop")],
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .expect("create mislabeled task");

        let load = load_loop_summaries(&pm).await.expect("load summaries");

        assert!(load.loops.is_empty(), "mislabeled task must be skipped");
        assert_eq!(load.warnings.len(), 1);
        assert!(
            load.warnings[0].contains("bd-mock-1") && load.warnings[0].contains("LoopSpec"),
            "warning should identify skipped issue and parse failure, got {:?}",
            load.warnings
        );
    }

    #[tokio::test]
    async fn load_loop_summaries_degrades_without_advanced_comments_api() {
        let inner = crate::plan::test_util::MockPm::new();
        let pm = NoAdvancedPm { inner };
        let loop_id = "noadvanced";
        create_loop_issue(
            &pm,
            loop_id,
            test_spec(loop_id, "Keep CI green"),
            vec![labels::loop_next_run_label(123)],
        )
        .await;

        let load = load_loop_summaries(&pm).await.expect("load summaries");

        assert_eq!(load.loops.len(), 1);
        assert_eq!(load.warnings.len(), 1);
        assert!(load.warnings[0].contains("advanced comments API is unavailable"));
        let summary = &load.loops[0];
        assert_eq!(summary.loop_id, loop_id);
        assert_eq!(summary.last_generation, None);
        assert_eq!(summary.last_outcome, None);
        assert_eq!(summary.last_cost_micros, None);
        assert_eq!(summary.consecutive_failures, 0);
        assert_eq!(summary.effective_interval_secs, summary.cadence_secs);
        assert!(!summary.backoff_active);
    }

    #[tokio::test]
    async fn loop_status_happy_path_reports_runs_and_backoff() {
        let pm = crate::plan::test_util::MockPm::new();
        let loop_id = "ciloop";
        let issue_id = create_loop_issue(
            &pm,
            loop_id,
            test_spec(loop_id, "Keep CI green"),
            vec![
                format!("{}l2", labels::AUTONOMY_PREFIX),
                labels::loop_next_run_label(10),
                labels::loop_next_run_label(20),
            ],
        )
        .await;
        add_loop_run(&pm, &issue_id, loop_id, 1, "approved", 100).await;
        add_loop_run(&pm, &issue_id, loop_id, 2, "failed", 200).await;
        add_loop_run(&pm, &issue_id, loop_id, 3, "failed", 300).await;

        let load = load_loop_summaries(&pm).await.expect("load summaries");
        let summary = load.loops.first().expect("summary");

        assert!(
            load.warnings.is_empty(),
            "unexpected warnings: {:?}",
            load.warnings
        );
        assert_eq!(summary.loop_id, loop_id);
        assert_eq!(summary.issue_id, issue_id);
        assert_eq!(summary.autonomy.as_deref(), Some("l2"));
        assert_eq!(summary.next_run, Some(20));
        assert_eq!(summary.last_generation, Some(3));
        assert_eq!(summary.last_outcome.as_deref(), Some("failed"));
        assert_eq!(summary.last_cost_micros, Some(300));
        assert_eq!(summary.consecutive_failures, 2);
        assert_eq!(summary.effective_interval_secs, 120);
        assert!(summary.backoff_active);
        assert_eq!(summary.goal_preview.as_deref(), Some("Keep CI green"));

        let status = build_loop_status(&pm, loop_id, 2)
            .await
            .expect("build status")
            .expect("loop status");
        let detail = status.to_detail_event();
        assert_eq!(detail.recent_runs.len(), 2);
        assert_eq!(detail.recent_runs[0].generation, 2);
        assert_eq!(detail.recent_runs[1].generation, 3);
        assert_eq!(detail.consecutive_failures, 2);
        assert_eq!(detail.effective_interval_secs, 120);
        assert!(detail.backoff_active);
    }
}
