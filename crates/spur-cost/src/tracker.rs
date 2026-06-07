use anyhow::Result;
use spur_acp::{CostTier, SessionId};
use std::path::Path;
use std::time::Duration;

use crate::db::{
    self, CostSummary, DelegationRecord, ModelCostSummary, ProjectCostSummary, SessionRecord,
    TokenSummary,
};
use crate::estimator::{estimate_cost, estimate_cost_from_tokens};
use crate::pricing::TokenUsage;
use crate::reporter::Reporter;
use crate::reports::{DailyReport, LiveReport, MonthlyReport, SessionReport, WeeklyReport};

/// High-level cost-tracking API used by the orchestrator.
///
/// Wraps a SQLite connection and exposes session/delegation lifecycle
/// methods plus aggregation queries.
pub struct CostTracker {
    conn: rusqlite::Connection,
}

impl CostTracker {
    /// Open (or create) the cost database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = db::init_db(db_path)?;
        Ok(Self { conn })
    }

    /// Record the start of a new session.
    // TODO: consolidate args into a SessionStartParams struct to reduce arity.
    #[expect(
        clippy::too_many_arguments,
        reason = "public tracker API mirrors session-start fields"
    )]
    pub fn start_session(
        &self,
        id: &SessionId,
        agent: &str,
        role: &str,
        parent: Option<&SessionId>,
        task: &str,
        project: Option<&str>,
        issue: Option<&str>,
    ) -> Result<()> {
        let record = SessionRecord {
            id: id.0.clone(),
            agent: agent.to_string(),
            role: role.to_string(),
            parent_session: parent.map(|p| p.0.clone()),
            task_summary: Some(task.to_string()),
            project: project.map(|s| s.to_string()),
            issue_ref: issue.map(|s| s.to_string()),
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            status: "running".to_string(),
            duration_seconds: None,
            estimated_cost_usd: None,
            model: None,
            input_tokens: None,
            output_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
        };
        db::insert_session(&self.conn, &record)
    }

    /// Record the end of a session, computing the estimated cost from the
    /// provided duration and cost tier.
    ///
    /// This is the legacy time-based estimator. Prefer [`Self::end_session_with_tokens`]
    /// for accurate token-based costing.
    pub fn end_session(
        &self,
        id: &SessionId,
        status: &str,
        duration: Duration,
        cost_tier: CostTier,
    ) -> Result<()> {
        let cost = estimate_cost(cost_tier, duration);
        db::update_session_end(&self.conn, &id.0, status, duration.as_secs() as i64, cost)
    }

    /// Record the end of a session with actual token usage.
    ///
    /// If `model` is provided and known in the pricing registry, cost is
    /// calculated from per-token rates. Otherwise falls back to the time-based
    /// estimate derived from `cost_tier` and `duration`.
    pub fn end_session_with_tokens(
        &self,
        id: &SessionId,
        status: &str,
        duration: Duration,
        cost_tier: CostTier,
        usage: TokenUsage,
        model: Option<&str>,
    ) -> Result<()> {
        let cost = estimate_cost_from_tokens(cost_tier, duration, usage, model);
        db::update_session_end_with_tokens(
            &self.conn,
            &id.0,
            status,
            duration.as_secs() as i64,
            cost,
            model,
            Some(usage.input_tokens as i64),
            Some(usage.output_tokens as i64),
            Some(usage.cache_creation_input_tokens as i64),
            Some(usage.cache_read_input_tokens as i64),
        )
    }

    /// Log a delegation from a brain session to a worker session.
    /// Returns the delegation log row ID for later completion.
    pub fn log_delegation(
        &self,
        brain: &SessionId,
        worker: &SessionId,
        agent: &str,
        task: &str,
    ) -> Result<i64> {
        let record = DelegationRecord {
            id: None,
            brain_session: brain.0.clone(),
            worker_session: worker.0.clone(),
            task: task.to_string(),
            agent: agent.to_string(),
            requested_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            status: "pending".to_string(),
            diff_stats: None,
        };
        db::insert_delegation(&self.conn, &record)
    }

    /// Mark a delegation as completed (or failed).
    pub fn complete_delegation(
        &self,
        log_id: i64,
        status: &str,
        diff_stats: Option<&str>,
    ) -> Result<()> {
        db::update_delegation_end(&self.conn, log_id, status, diff_stats)
    }

    /// Cost summary for today, grouped by agent.
    pub fn today_summary(&self) -> Result<Vec<CostSummary>> {
        db::query_cost_today(&self.conn)
    }

    /// Cost summary for the last 7 days, grouped by agent.
    pub fn week_summary(&self) -> Result<Vec<CostSummary>> {
        let now = chrono::Utc::now();
        let week_ago = now - chrono::TimeDelta::days(7);
        db::query_cost_range(&self.conn, &week_ago.to_rfc3339(), &now.to_rfc3339())
    }

    /// Cost summary grouped by project (all time).
    pub fn by_project(&self) -> Result<Vec<ProjectCostSummary>> {
        db::query_cost_by_project(&self.conn)
    }

    /// Retrieve full details for a single session.
    pub fn session_detail(&self, id: &SessionId) -> Result<Option<SessionRecord>> {
        db::query_session(&self.conn, &id.0)
    }

    /// Return the most recent sessions (up to `limit`).
    pub fn recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        db::query_recent_sessions(&self.conn, limit)
    }

    /// Return all delegations where the given session is either the brain or the worker.
    pub fn session_delegations(&self, id: &SessionId) -> Result<Vec<DelegationRecord>> {
        db::query_delegations_for_session(&self.conn, &id.0)
    }

    // ─── Token-based Queries ──────────────────────────────────────────

    /// Token summary for today, grouped by agent.
    pub fn today_token_summary(&self) -> Result<Vec<TokenSummary>> {
        db::query_tokens_today(&self.conn)
    }

    /// Token summary for the last 7 days, grouped by agent.
    pub fn week_token_summary(&self) -> Result<Vec<TokenSummary>> {
        let now = chrono::Utc::now();
        let week_ago = now - chrono::TimeDelta::days(7);
        db::query_tokens_range(&self.conn, &week_ago.to_rfc3339(), &now.to_rfc3339())
    }

    /// Cost summary grouped by model and agent.
    pub fn by_model(&self) -> Result<Vec<ModelCostSummary>> {
        db::query_cost_by_model(&self.conn)
    }

    // ─── Reporter Integration ─────────────────────────────────────────

    /// Obtain a [`Reporter`] that reads agent-native session files.
    ///
    /// The reporter discovers and parses JSONL logs from Claude, Codex,
    /// and Kiro data directories. It does not query the SQLite database.
    ///
    /// # Example
    /// ```no_run
    /// use spur_cost::CostTracker;
    /// use spur_cost::reporter::ReportRange;
    /// use spur_cost::presenter::{Presenter, table::TablePresenter};
    ///
    /// let tracker = CostTracker::open(std::path::Path::new("cost.db")).unwrap();
    /// let reporter = tracker.reporter();
    /// let daily = reporter.daily_report(ReportRange::today()).unwrap();
    /// let presenter = TablePresenter::new();
    /// println!("{}", presenter.render_daily(&daily));
    /// ```
    pub fn reporter(&self) -> Reporter {
        Reporter::new()
    }

    /// Convenience: daily report for today.
    pub fn daily_report(&self) -> Result<Vec<DailyReport>> {
        self.reporter().today()
    }

    /// Convenience: weekly report for the last 7 days.
    pub fn weekly_report(&self) -> Result<Vec<WeeklyReport>> {
        self.reporter().last_week()
    }

    /// Convenience: monthly report for the last 30 days.
    pub fn monthly_report(&self) -> Result<Vec<MonthlyReport>> {
        self.reporter().last_month()
    }

    /// Convenience: session report for all time.
    pub fn session_report(&self) -> Result<SessionReport> {
        self.reporter()
            .session_report(crate::reporter::ReportRange::all_time())
    }

    /// Convenience: live report of active sessions.
    pub fn live_report(&self) -> Result<LiveReport> {
        self.reporter().live_report(30)
    }
}
