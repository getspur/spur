use anyhow::Result;
use spur_acp::{CostTier, SessionId};
use std::path::Path;
use std::time::Duration;

use crate::db::{
    self, CostSummary, DelegationRecord, ProjectCostSummary, SessionRecord,
};
use crate::estimator::estimate_cost;

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
        };
        db::insert_session(&self.conn, &record)
    }

    /// Record the end of a session, computing the estimated cost from the
    /// provided duration and cost tier.
    pub fn end_session(
        &self,
        id: &SessionId,
        status: &str,
        duration: Duration,
        cost_tier: CostTier,
    ) -> Result<()> {
        let cost = estimate_cost(cost_tier, duration);
        db::update_session_end(
            &self.conn,
            &id.0,
            status,
            duration.as_secs() as i64,
            cost,
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
        db::query_cost_range(
            &self.conn,
            &week_ago.to_rfc3339(),
            &now.to_rfc3339(),
        )
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
}
