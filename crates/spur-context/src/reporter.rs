//! High-level reporter over the DuckDB analytics engine.
//!
//! Provides range-based reports with totals and breakdowns,
//! similar to `spur_cost::Reporter` but backed by DuckDB views.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use std::collections::HashMap;

use crate::engine::{
    AnalyticsEngine, DailyRow, ModelRow, MonthlyRow, ProjectRow, SessionRow, WeeklyRow,
};

// ─── ReportRange ──────────────────────────────────────────────────────

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

// ─── Breakdown & Total Types ──────────────────────────────────────────

/// Summary totals across all entries in a report window.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Totals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub session_count: i64,
}

impl Totals {
    pub fn from_daily_rows(rows: &[DailyRow]) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.input_tokens += r.input_tokens;
            t.output_tokens += r.output_tokens;
            t.cache_read_tokens += r.cache_read_tokens;
            t.cache_creation_tokens += r.cache_creation_tokens;
            t.cost_usd += r.cost_usd;
            t.session_count += r.sessions;
        }
        t.total_tokens =
            t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_creation_tokens;
        t
    }

    pub fn from_weekly_rows(rows: &[WeeklyRow]) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.input_tokens += r.input_tokens;
            t.output_tokens += r.output_tokens;
            t.cache_read_tokens += r.cache_read_tokens;
            t.cache_creation_tokens += r.cache_creation_tokens;
            t.cost_usd += r.cost_usd;
            t.session_count += r.sessions;
        }
        t.total_tokens =
            t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_creation_tokens;
        t
    }

    pub fn from_monthly_rows(rows: &[MonthlyRow]) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.input_tokens += r.input_tokens;
            t.output_tokens += r.output_tokens;
            t.cache_read_tokens += r.cache_read_tokens;
            t.cache_creation_tokens += r.cache_creation_tokens;
            t.cost_usd += r.cost_usd;
            t.session_count += r.sessions;
        }
        t.total_tokens =
            t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_creation_tokens;
        t
    }
}

/// Aggregated usage for a single agent within a report window.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentBreakdown {
    pub agent: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub session_count: i64,
}

impl From<&DailyRow> for AgentBreakdown {
    fn from(r: &DailyRow) -> Self {
        Self {
            agent: r.agent.clone(),
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_tokens: r.cache_read_tokens,
            cache_creation_tokens: r.cache_creation_tokens,
            cost_usd: r.cost_usd,
            session_count: r.sessions,
        }
    }
}

impl From<&WeeklyRow> for AgentBreakdown {
    fn from(r: &WeeklyRow) -> Self {
        Self {
            agent: r.agent.clone(),
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_tokens: r.cache_read_tokens,
            cache_creation_tokens: r.cache_creation_tokens,
            cost_usd: r.cost_usd,
            session_count: r.sessions,
        }
    }
}

impl From<&MonthlyRow> for AgentBreakdown {
    fn from(r: &MonthlyRow) -> Self {
        Self {
            agent: r.agent.clone(),
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_tokens: r.cache_read_tokens,
            cache_creation_tokens: r.cache_creation_tokens,
            cost_usd: r.cost_usd,
            session_count: r.sessions,
        }
    }
}

// ─── Report Types ─────────────────────────────────────────────────────

/// Usage aggregated by calendar day.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyReport {
    pub date: NaiveDate,
    pub agent_rows: Vec<DailyRow>,
    pub totals: Totals,
    pub agent_breakdowns: Vec<AgentBreakdown>,
}

/// Usage aggregated by ISO week (Monday-based).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WeeklyReport {
    pub week_start: NaiveDate,
    pub agent_rows: Vec<WeeklyRow>,
    pub totals: Totals,
    pub agent_breakdowns: Vec<AgentBreakdown>,
}

/// Usage aggregated by calendar month.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MonthlyReport {
    pub year_month: String,
    pub agent_rows: Vec<MonthlyRow>,
    pub totals: Totals,
    pub agent_breakdowns: Vec<AgentBreakdown>,
}

/// Totals for a model breakdown report.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_cost: f64,
}

impl ModelTotals {
    pub fn from_rows(rows: &[ModelRow]) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.requests += r.requests;
            t.input_tokens += r.input_tokens;
            t.output_tokens += r.output_tokens;
            t.total_cost += r.total_cost;
        }
        t
    }
}

/// Model cost breakdown report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelReport {
    pub rows: Vec<ModelRow>,
    pub totals: ModelTotals,
}

/// Totals for a project breakdown report.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectTotals {
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

impl ProjectTotals {
    pub fn from_rows(rows: &[ProjectRow]) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.sessions += r.sessions;
            t.input_tokens += r.input_tokens;
            t.output_tokens += r.output_tokens;
            t.cost_usd += r.cost_usd;
        }
        t
    }
}

/// Project cost breakdown report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectReport {
    pub rows: Vec<ProjectRow>,
    pub totals: ProjectTotals,
}

/// Detail for a single session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionReport {
    pub session: SessionRow,
}

/// Burn-rate metrics for a live (active) session block.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BurnRate {
    pub tokens_per_minute: f64,
    pub cost_per_hour: f64,
    pub observed_seconds: u64,
}

/// A live session block with burn-rate projection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveBlock {
    pub session_id: String,
    pub agent: String,
    pub models: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub burn_rate: Option<BurnRate>,
    pub projected_cost: Option<f64>,
}

/// Live usage report showing currently active sessions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveReport {
    pub blocks: Vec<LiveBlock>,
    pub totals: Totals,
}

// ─── Reporter ─────────────────────────────────────────────────────────

/// High-level reporter backed by DuckDB.
pub struct Reporter {
    engine: AnalyticsEngine,
}

impl Reporter {
    /// Create a reporter from an initialized engine.
    pub fn new(engine: AnalyticsEngine) -> Self {
        Self { engine }
    }

    /// Access the underlying engine.
    pub fn engine(&self) -> &AnalyticsEngine {
        &self.engine
    }

    /// Consume the reporter and return the underlying engine.
    pub fn into_engine(self) -> AnalyticsEngine {
        self.engine
    }

    /// Generate a daily usage report for the given time range.
    pub fn daily_report(&self, range: ReportRange) -> Result<Vec<DailyReport>> {
        let end = range
            .to
            .date_naive()
            .succ_opt()
            .unwrap_or(range.to.date_naive());
        let rows = self
            .engine
            .daily_report_range(range.from.date_naive(), end)?;
        let mut groups: HashMap<String, Vec<DailyRow>> = HashMap::new();
        for row in rows {
            groups.entry(row.day.clone()).or_default().push(row);
        }

        let mut reports: Vec<DailyReport> = groups
            .into_iter()
            .map(|(day, day_rows)| {
                let totals = Totals::from_daily_rows(&day_rows);
                let agents: Vec<AgentBreakdown> =
                    day_rows.iter().map(AgentBreakdown::from).collect();
                let date = NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                    .with_context(|| format!("invalid day format: {}", day))?;
                Ok(DailyReport {
                    date,
                    agent_rows: day_rows,
                    totals,
                    agent_breakdowns: agents,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        reports.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(reports)
    }

    /// Generate a weekly usage report for the given time range.
    pub fn weekly_report(&self, range: ReportRange) -> Result<Vec<WeeklyReport>> {
        let end = range
            .to
            .date_naive()
            .succ_opt()
            .unwrap_or(range.to.date_naive());
        let rows = self
            .engine
            .weekly_report_range(range.from.date_naive(), end)?;
        let mut groups: HashMap<String, Vec<WeeklyRow>> = HashMap::new();
        for row in rows {
            groups.entry(row.week.clone()).or_default().push(row);
        }

        let mut reports: Vec<WeeklyReport> = groups
            .into_iter()
            .map(|(week, week_rows)| {
                let totals = Totals::from_weekly_rows(&week_rows);
                let agents: Vec<AgentBreakdown> =
                    week_rows.iter().map(AgentBreakdown::from).collect();
                let week_start = NaiveDate::parse_from_str(&week, "%Y-%m-%d")
                    .with_context(|| format!("invalid week format: {}", week))?;
                Ok(WeeklyReport {
                    week_start,
                    agent_rows: week_rows,
                    totals,
                    agent_breakdowns: agents,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        reports.sort_by(|a, b| b.week_start.cmp(&a.week_start));
        Ok(reports)
    }

    /// Generate a monthly usage report for the given time range.
    pub fn monthly_report(&self, range: ReportRange) -> Result<Vec<MonthlyReport>> {
        let end = range
            .to
            .date_naive()
            .succ_opt()
            .unwrap_or(range.to.date_naive());
        let rows = self
            .engine
            .monthly_report_range(range.from.date_naive(), end)?;
        let mut groups: HashMap<String, Vec<MonthlyRow>> = HashMap::new();
        for row in rows {
            groups.entry(row.month.clone()).or_default().push(row);
        }

        let mut reports: Vec<MonthlyReport> = groups
            .into_iter()
            .map(|(month, month_rows)| {
                let totals = Totals::from_monthly_rows(&month_rows);
                let agents: Vec<AgentBreakdown> =
                    month_rows.iter().map(AgentBreakdown::from).collect();
                Ok(MonthlyReport {
                    year_month: month,
                    agent_rows: month_rows,
                    totals,
                    agent_breakdowns: agents,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        reports.sort_by(|a, b| b.year_month.cmp(&a.year_month));
        Ok(reports)
    }

    /// Model cost breakdown.
    pub fn model_breakdown(&self) -> Result<ModelReport> {
        let rows = self.engine.model_breakdown()?;
        let totals = ModelTotals::from_rows(&rows);
        Ok(ModelReport { rows, totals })
    }

    /// Project cost breakdown.
    pub fn project_breakdown(&self) -> Result<ProjectReport> {
        let rows = self.engine.project_breakdown()?;
        let totals = ProjectTotals::from_rows(&rows);
        Ok(ProjectReport { rows, totals })
    }

    /// Generate a session report for a single session ID.
    pub fn session_report(&self, session_id: &str) -> Result<Option<SessionReport>> {
        Ok(self
            .engine
            .session_detail(session_id)?
            .map(|s| SessionReport { session: s }))
    }

    /// Generate a live report of recent activity.
    pub fn live_report(&self, active_window_minutes: i64) -> Result<LiveReport> {
        let rows: Vec<crate::engine::LiveBlockRow> = self
            .engine
            .live_recent_sessions(active_window_minutes.max(0) as u32)?;
        let mut blocks = Vec::new();
        let mut grand_totals = Totals::default();

        for row in rows {
            let started_at = row.started_at.as_ref().and_then(|s| parse_timestamp(s));
            let last_activity = row.last_activity.as_ref().and_then(|s| parse_timestamp(s));

            let dur_sec = match (&started_at, &last_activity) {
                (Some(s), Some(e)) => e.signed_duration_since(*s).num_seconds().max(0) as u64,
                _ => 0,
            };

            let burn_rate = if dur_sec > 0 {
                let minutes = dur_sec as f64 / 60.0;
                let total_tokens = row.input_tokens
                    + row.output_tokens
                    + row.cache_read_tokens
                    + row.cache_creation_tokens;
                Some(BurnRate {
                    tokens_per_minute: total_tokens as f64 / minutes,
                    cost_per_hour: row.cost_usd / minutes * 60.0,
                    observed_seconds: dur_sec,
                })
            } else {
                None
            };

            let projected_cost = burn_rate.as_ref().map(|br| br.cost_per_hour);
            let is_active = last_activity
                .map(|la| Utc::now().signed_duration_since(la).num_seconds() < 300)
                .unwrap_or(false);

            grand_totals.input_tokens += row.input_tokens;
            grand_totals.output_tokens += row.output_tokens;
            grand_totals.cache_read_tokens += row.cache_read_tokens;
            grand_totals.cache_creation_tokens += row.cache_creation_tokens;
            grand_totals.cost_usd += row.cost_usd;
            grand_totals.session_count += 1;

            blocks.push(LiveBlock {
                session_id: row.session_id,
                agent: row.agent,
                models: row.models,
                started_at,
                last_activity,
                is_active,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cache_read_tokens: row.cache_read_tokens,
                cache_creation_tokens: row.cache_creation_tokens,
                cost_usd: row.cost_usd,
                burn_rate,
                projected_cost,
            });
        }

        grand_totals.total_tokens = grand_totals.input_tokens
            + grand_totals.output_tokens
            + grand_totals.cache_read_tokens
            + grand_totals.cache_creation_tokens;

        Ok(LiveReport {
            blocks,
            totals: grand_totals,
        })
    }

    /// Daily report for today.
    pub fn today(&self) -> Result<Vec<DailyReport>> {
        self.daily_report(ReportRange::today())
    }

    /// Weekly report for the last 7 days.
    pub fn last_week(&self) -> Result<Vec<WeeklyReport>> {
        self.weekly_report(ReportRange::last_days(7))
    }

    /// Monthly report for the last 30 days.
    pub fn last_month(&self) -> Result<Vec<MonthlyReport>> {
        self.monthly_report(ReportRange::last_days(30))
    }
}

fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        })
        .or_else(|| {
            DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        })
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;
    use crate::engine::AnalyticsEngine;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_engine_with_claude_data(tmp: &TempDir) -> AnalyticsEngine {
        let claude_dir = tmp.path().join("claude/projects/spur");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let jsonl_path = claude_dir.join("events.jsonl");
        let mut file = std::fs::File::create(&jsonl_path).unwrap();

        // Two events on 2026-04-23
        writeln!(
            file,
            r#"{{"timestamp":"2026-04-23T10:00:00Z","sessionId":"sess-1","costUSD":0.05,"tokenUsage":{{"inputTokens":1000,"outputTokens":500,"cacheReadTokens":200,"cacheCreationTokens":0}},"model":"claude-sonnet-4","project":"spur"}}"#
        ).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-04-23T11:00:00Z","sessionId":"sess-2","costUSD":0.10,"tokenUsage":{{"inputTokens":2000,"outputTokens":1000,"cacheReadTokens":400,"cacheCreationTokens":0}},"model":"claude-sonnet-4","project":"spur"}}"#
        ).unwrap();
        // One event on 2026-04-22
        writeln!(
            file,
            r#"{{"timestamp":"2026-04-22T10:00:00Z","sessionId":"sess-3","costUSD":0.03,"tokenUsage":{{"inputTokens":500,"outputTokens":250,"cacheReadTokens":100,"cacheCreationTokens":0}},"model":"claude-sonnet-4","project":"spur"}}"#
        ).unwrap();

        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();

        engine
            .conn()
            .execute_batch(&format!(
                "CREATE OR REPLACE VIEW claude_events AS \
             SELECT \
                timestamp::TIMESTAMP AS timestamp, \
                sessionId AS session_id, \
                'claude' AS agent, \
                NULLIF(model, '<synthetic>') AS model, \
                project, \
                tokenUsage.inputTokens AS input_tokens, \
                tokenUsage.outputTokens AS output_tokens, \
                tokenUsage.cacheReadTokens AS cache_read_tokens, \
                tokenUsage.cacheCreationTokens AS cache_creation_tokens, \
                costUSD AS cost_usd \
             FROM read_json_auto('{}', ignore_errors=true)",
                jsonl_path.to_str().unwrap().replace('\\', "/")
            ))
            .unwrap();

        engine
    }

    fn claude_fixture_range() -> ReportRange {
        ReportRange {
            from: NaiveDate::from_ymd_opt(2026, 4, 22)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
            to: NaiveDate::from_ymd_opt(2026, 4, 24)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
        }
    }

    #[test]
    fn test_reporter_daily_grouping() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let reports = reporter.daily_report(claude_fixture_range()).unwrap();
        assert_eq!(reports.len(), 2, "expected 2 days");

        // Most recent first
        assert_eq!(
            reports[0].date,
            NaiveDate::from_ymd_opt(2026, 4, 23).unwrap()
        );
        assert_eq!(reports[0].agent_rows.len(), 1);
        assert_eq!(reports[0].totals.input_tokens, 3000);
        assert!((reports[0].totals.cost_usd - 0.15).abs() < 0.001);

        assert_eq!(
            reports[1].date,
            NaiveDate::from_ymd_opt(2026, 4, 22).unwrap()
        );
        assert_eq!(reports[1].totals.input_tokens, 500);
        assert!((reports[1].totals.cost_usd - 0.03).abs() < 0.001);
    }

    #[test]
    fn test_reporter_weekly_grouping() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let reports = reporter.weekly_report(claude_fixture_range()).unwrap();
        assert_eq!(reports.len(), 1);
        // 2026-04-20 is Monday of that week
        assert_eq!(
            reports[0].week_start,
            NaiveDate::from_ymd_opt(2026, 4, 20).unwrap()
        );
        assert_eq!(reports[0].totals.input_tokens, 3500);
    }

    #[test]
    fn test_reporter_monthly_grouping() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let reports = reporter.monthly_report(claude_fixture_range()).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].year_month, "2026-04");
        assert_eq!(reports[0].totals.input_tokens, 3500);
    }

    #[test]
    fn test_reporter_live_report_large_window() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let live = reporter.live_report(1_000_000).unwrap();
        assert_eq!(live.blocks.len(), 3);
        assert!((live.totals.cost_usd - 0.18).abs() < 0.001);
    }

    #[test]
    fn test_reporter_session_report() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let report = reporter.session_report("sess-1").unwrap();
        assert!(report.is_some());
        let session = report.unwrap().session;
        assert_eq!(session.session_id, "sess-1");
        assert_eq!(session.agent, "claude");
        assert_eq!(session.input_tokens, 1000);
        assert_eq!(session.output_tokens, 500);
        assert_eq!(session.events, 1);
        // Duration should be 0 since only one event
        assert_eq!(session.duration_seconds, Some(0));
    }

    #[test]
    fn test_reporter_model_breakdown() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let report = reporter.model_breakdown().unwrap();
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].model, "claude-sonnet-4");
        assert_eq!(report.rows[0].agent, "claude");
        assert_eq!(report.rows[0].requests, 3);
        assert_eq!(report.totals.requests, 3);
        assert_eq!(report.totals.input_tokens, report.rows[0].input_tokens);
    }

    #[test]
    fn test_reporter_project_breakdown() {
        let tmp = TempDir::new().unwrap();
        let engine = setup_engine_with_claude_data(&tmp);
        let reporter = Reporter::new(engine);

        let report = reporter.project_breakdown().unwrap();
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].project, "spur");
        assert_eq!(report.rows[0].sessions, 3);
        assert_eq!(report.totals.sessions, 3);
        assert_eq!(report.totals.cost_usd, report.rows[0].cost_usd);
    }

    #[test]
    fn test_reporter_engine_accessor() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();

        let reporter = Reporter::new(engine);
        // Should be able to access underlying engine
        let _conn = reporter.engine().conn();
        // Should be able to recover engine
        let _engine = reporter.into_engine();
    }

    #[test]
    fn test_reporter_empty() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();

        let reporter = Reporter::new(engine);
        let reports = reporter.today().unwrap();
        assert!(reports.is_empty());
    }
}
