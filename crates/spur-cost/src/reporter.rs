//! Report engine: load agent session data and produce structured reports.
//!
//! This reporter reads usage data from agent-native JSONL files (Claude,
//! Codex, Kiro) via the ingestion pipeline, calculates costs, and produces
//! daily, weekly, monthly, session, and live reports.

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::ingest::{IngestionPipeline, TokenEvent};
use crate::pricing::{calculate_cost_for_model, PricingRegistry};
use crate::reports::{
    aggregate_by_agent, aggregate_by_model, aggregate_by_project, build_session_tree, group_by_day,
    group_by_month, group_by_week, BurnRate, DailyReport, LiveBlock, LiveReport, MonthlyReport,
    SessionReport, Totals, WeeklyReport,
};

/// Time range filter for reports.
#[derive(Debug, Clone, Copy)]
pub struct ReportRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl ReportRange {
    /// Today (00:00 UTC to now).
    pub fn today() -> Self {
        let now = Utc::now();
        let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
        Self {
            from: start,
            to: now,
        }
    }

    /// Last N days.
    pub fn last_days(n: i64) -> Self {
        let now = Utc::now();
        let start = now - ChronoDuration::days(n);
        Self {
            from: start,
            to: now,
        }
    }

    /// Last N weeks.
    pub fn last_weeks(n: i64) -> Self {
        let now = Utc::now();
        let start = now - ChronoDuration::weeks(n);
        Self {
            from: start,
            to: now,
        }
    }

    /// All time.
    pub fn all_time() -> Self {
        Self {
            from: DateTime::UNIX_EPOCH,
            to: Utc::now(),
        }
    }
}

/// Report engine that reads agent session data.
pub struct Reporter {
    pipeline: IngestionPipeline,
    pricing: PricingRegistry,
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter {
    /// Create a reporter with the default ingestion pipeline and built-in pricing.
    pub fn new() -> Self {
        Self {
            pipeline: IngestionPipeline::with_defaults(),
            pricing: PricingRegistry::with_builtin_prices(),
        }
    }

    /// Create a reporter with a custom pipeline.
    pub fn with_pipeline(pipeline: IngestionPipeline) -> Self {
        Self {
            pipeline,
            pricing: PricingRegistry::with_builtin_prices(),
        }
    }

    // ─── Cost Calculation ─────────────────────────────────────────────

    /// Ensure every event has a cost. If the agent provided a pre-calculated
    /// cost, use it. Otherwise compute from tokens + model pricing.
    fn compute_costs(&self, events: &mut [TokenEvent]) {
        for e in events.iter_mut() {
            if e.cost_usd.is_some() {
                continue;
            }
            if let Some(model) = &e.model {
                let usage = crate::pricing::TokenUsage {
                    input_tokens: e.input_tokens,
                    output_tokens: e.output_tokens,
                    cache_creation_input_tokens: e.cache_creation_tokens,
                    cache_read_input_tokens: e.cache_read_tokens,
                };
                if let Some(cost) = calculate_cost_for_model(usage, model, &self.pricing) {
                    e.cost_usd = Some(cost);
                }
            }
        }
    }

    // ─── Daily Report ─────────────────────────────────────────────────

    /// Generate a daily usage report for the given time range.
    pub fn daily_report(&self, range: ReportRange) -> anyhow::Result<Vec<DailyReport>> {
        let mut entries = self.pipeline.load_range(range.from, range.to)?;
        self.compute_costs(&mut entries);
        let grouped = group_by_day(entries);

        let mut reports: Vec<DailyReport> = grouped
            .into_iter()
            .map(|(date, day_entries)| {
                let totals = Totals::from_entries(&day_entries);
                let agents = aggregate_by_agent(&day_entries);
                let models = aggregate_by_model(&day_entries);
                let projects = aggregate_by_project(&day_entries);
                DailyReport {
                    date,
                    entries: day_entries,
                    agent_breakdowns: agents,
                    model_breakdowns: models,
                    project_breakdowns: projects,
                    totals,
                }
            })
            .collect();

        reports.sort_by_key(|report| std::cmp::Reverse(report.date));
        Ok(reports)
    }

    // ─── Weekly Report ────────────────────────────────────────────────

    /// Generate a weekly usage report for the given time range.
    pub fn weekly_report(&self, range: ReportRange) -> anyhow::Result<Vec<WeeklyReport>> {
        let mut entries = self.pipeline.load_range(range.from, range.to)?;
        self.compute_costs(&mut entries);
        let grouped = group_by_week(entries);

        let mut reports: Vec<WeeklyReport> = grouped
            .into_iter()
            .map(|(week_start, week_entries)| {
                let totals = Totals::from_entries(&week_entries);
                let agents = aggregate_by_agent(&week_entries);
                let models = aggregate_by_model(&week_entries);
                let projects = aggregate_by_project(&week_entries);
                WeeklyReport {
                    week_start,
                    entries: week_entries,
                    agent_breakdowns: agents,
                    model_breakdowns: models,
                    project_breakdowns: projects,
                    totals,
                }
            })
            .collect();

        reports.sort_by_key(|report| std::cmp::Reverse(report.week_start));
        Ok(reports)
    }

    // ─── Monthly Report ───────────────────────────────────────────────

    /// Generate a monthly usage report for the given time range.
    pub fn monthly_report(&self, range: ReportRange) -> anyhow::Result<Vec<MonthlyReport>> {
        let mut entries = self.pipeline.load_range(range.from, range.to)?;
        self.compute_costs(&mut entries);
        let grouped = group_by_month(entries);

        let mut reports: Vec<MonthlyReport> = grouped
            .into_iter()
            .map(|(year_month, month_entries)| {
                let totals = Totals::from_entries(&month_entries);
                let agents = aggregate_by_agent(&month_entries);
                let models = aggregate_by_model(&month_entries);
                let projects = aggregate_by_project(&month_entries);
                MonthlyReport {
                    year_month,
                    entries: month_entries,
                    agent_breakdowns: agents,
                    model_breakdowns: models,
                    project_breakdowns: projects,
                    totals,
                }
            })
            .collect();

        reports.sort_by(|a, b| b.year_month.cmp(&a.year_month));
        Ok(reports)
    }

    // ─── Session Report ───────────────────────────────────────────────

    /// Generate a session report grouping events by session_id.
    pub fn session_report(&self, range: ReportRange) -> anyhow::Result<SessionReport> {
        let mut entries = self.pipeline.load_range(range.from, range.to)?;
        self.compute_costs(&mut entries);
        let roots = build_session_tree(entries.clone());

        let totals = Totals::from_entries(&entries);
        let agents = aggregate_by_agent(&entries);
        let models = aggregate_by_model(&entries);
        let projects = aggregate_by_project(&entries);

        Ok(SessionReport {
            roots,
            agent_breakdowns: agents,
            model_breakdowns: models,
            project_breakdowns: projects,
            totals,
        })
    }

    // ─── Live Report ──────────────────────────────────────────────────

    /// Generate a live report of recent activity.
    ///
    /// A block is a session that has activity within `active_window_minutes`.
    /// Burn rate is computed from the session's total tokens and an assumed
    /// observation window (the session duration is unknown from file data, so
    /// we use the time span between first and last event in the session).
    pub fn live_report(&self, active_window_minutes: i64) -> anyhow::Result<LiveReport> {
        let cutoff = Utc::now() - ChronoDuration::minutes(active_window_minutes);
        let mut entries = self.pipeline.load_all()?;
        self.compute_costs(&mut entries);

        // Filter to recent events
        let recent: Vec<_> = entries
            .into_iter()
            .filter(|e| e.timestamp >= cutoff)
            .collect();

        // Group by session_id and build blocks
        let mut by_session: std::collections::HashMap<String, Vec<TokenEvent>> =
            std::collections::HashMap::new();
        for e in recent {
            let sid = e
                .session_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            by_session.entry(sid).or_default().push(e);
        }

        let mut blocks = Vec::new();
        for (sid, mut sess_events) in by_session {
            sess_events.sort_by_key(|event| event.timestamp);
            let first = sess_events.first().unwrap();
            let last = sess_events.last().unwrap();
            let totals = Totals::from_entries(&sess_events);

            let dur_sec = (last.timestamp - first.timestamp).num_seconds().max(0) as u64;
            let cost = totals.cost_usd;

            let burn_rate = if dur_sec > 0 {
                let minutes = dur_sec as f64 / 60.0;
                Some(BurnRate {
                    tokens_per_minute: totals.total_tokens as f64 / minutes,
                    cost_per_hour: cost / minutes * 60.0,
                    observed_seconds: dur_sec,
                })
            } else {
                None
            };

            let projected_cost = burn_rate.as_ref().map(|br| br.cost_per_hour);

            blocks.push(LiveBlock {
                session_id: sid,
                agent: first.agent.clone(),
                model: first.model.clone(),
                project: first.project.clone(),
                started_at: first.timestamp,
                last_activity: last.timestamp,
                is_active: dur_sec < 300, // active if < 5 min since last event
                input_tokens: totals.input_tokens,
                output_tokens: totals.output_tokens,
                cache_creation_tokens: totals.cache_creation_tokens,
                cache_read_tokens: totals.cache_read_tokens,
                cost_usd: cost,
                burn_rate,
                projected_cost,
            });
        }

        // Sort by cost descending
        blocks.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let grand_totals = Totals::from_entries(
            &blocks
                .iter()
                .map(|b| TokenEvent {
                    timestamp: b.started_at,
                    session_id: Some(b.session_id.clone()),
                    agent: b.agent.clone(),
                    model: b.model.clone(),
                    project: b.project.clone(),
                    input_tokens: b.input_tokens,
                    output_tokens: b.output_tokens,
                    cache_creation_tokens: b.cache_creation_tokens,
                    cache_read_tokens: b.cache_read_tokens,
                    cost_usd: Some(b.cost_usd),
                    source_file: std::path::PathBuf::new(),
                })
                .collect::<Vec<_>>(),
        );

        Ok(LiveReport {
            blocks,
            totals: grand_totals,
        })
    }

    // ─── Convenience Shortcuts ────────────────────────────────────────

    /// Daily report for today.
    pub fn today(&self) -> anyhow::Result<Vec<DailyReport>> {
        self.daily_report(ReportRange::today())
    }

    /// Weekly report for the last 7 days.
    pub fn last_week(&self) -> anyhow::Result<Vec<WeeklyReport>> {
        self.weekly_report(ReportRange::last_days(7))
    }

    /// Monthly report for the last 30 days.
    pub fn last_month(&self) -> anyhow::Result<Vec<MonthlyReport>> {
        self.monthly_report(ReportRange::last_days(30))
    }
}
