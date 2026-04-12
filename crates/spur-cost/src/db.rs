use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

// ─── Record Types ─────────────────────────────────────────────────────

/// A tracked session (brain or worker).
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub agent: String,
    pub role: String,
    pub parent_session: Option<String>,
    pub task_summary: Option<String>,
    pub project: Option<String>,
    pub issue_ref: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub duration_seconds: Option<i64>,
    pub estimated_cost_usd: Option<f64>,
}

/// A logged delegation from brain to worker.
#[derive(Debug, Clone)]
pub struct DelegationRecord {
    pub id: Option<i64>,
    pub brain_session: String,
    pub worker_session: String,
    pub task: String,
    pub agent: String,
    pub requested_at: String,
    pub completed_at: Option<String>,
    pub status: String,
    pub diff_stats: Option<String>,
}

/// Cost summary grouped by agent.
#[derive(Debug, Clone)]
pub struct CostSummary {
    pub agent: String,
    pub total_cost_usd: f64,
    pub session_count: i64,
    pub total_duration_seconds: i64,
}

/// Cost summary grouped by project.
#[derive(Debug, Clone)]
pub struct ProjectCostSummary {
    pub project: String,
    pub total_cost_usd: f64,
    pub session_count: i64,
}

// ─── Database Initialization ──────────────────────────────────────────

/// Open (or create) the SQLite database at `path` and ensure the schema exists.
pub fn init_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            role TEXT NOT NULL,
            parent_session TEXT,
            task_summary TEXT,
            project TEXT,
            issue_ref TEXT,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            duration_seconds INTEGER,
            estimated_cost_usd REAL
        );

        CREATE TABLE IF NOT EXISTS delegation_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            brain_session TEXT NOT NULL,
            worker_session TEXT NOT NULL,
            task TEXT NOT NULL,
            agent TEXT NOT NULL,
            requested_at TEXT NOT NULL,
            completed_at TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            diff_stats TEXT
        );
        ",
    )?;

    Ok(conn)
}

// ─── Session Queries ──────────────────────────────────────────────────

/// Insert a new session record.
pub fn insert_session(conn: &Connection, session: &SessionRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (
            id, agent, role, parent_session, task_summary, project,
            issue_ref, started_at, ended_at, status, duration_seconds,
            estimated_cost_usd
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            session.id,
            session.agent,
            session.role,
            session.parent_session,
            session.task_summary,
            session.project,
            session.issue_ref,
            session.started_at,
            session.ended_at,
            session.status,
            session.duration_seconds,
            session.estimated_cost_usd,
        ],
    )?;
    Ok(())
}

/// Update a session when it ends: set status, duration, and estimated cost.
pub fn update_session_end(
    conn: &Connection,
    id: &str,
    status: &str,
    duration: i64,
    cost: f64,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE sessions SET ended_at = ?1, status = ?2, duration_seconds = ?3,
         estimated_cost_usd = ?4 WHERE id = ?5",
        params![now, status, duration, cost, id],
    )?;
    Ok(())
}

/// Retrieve a single session and attach its delegations (as brain or worker).
pub fn query_session(conn: &Connection, id: &str) -> Result<Option<SessionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent, role, parent_session, task_summary, project,
                issue_ref, started_at, ended_at, status, duration_seconds,
                estimated_cost_usd
         FROM sessions WHERE id = ?1",
    )?;

    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(SessionRecord {
            id: row.get(0)?,
            agent: row.get(1)?,
            role: row.get(2)?,
            parent_session: row.get(3)?,
            task_summary: row.get(4)?,
            project: row.get(5)?,
            issue_ref: row.get(6)?,
            started_at: row.get(7)?,
            ended_at: row.get(8)?,
            status: row.get(9)?,
            duration_seconds: row.get(10)?,
            estimated_cost_usd: row.get(11)?,
        })),
        None => Ok(None),
    }
}

// ─── Delegation Queries ───────────────────────────────────────────────

/// Insert a delegation log entry. Returns the auto-generated row ID.
pub fn insert_delegation(conn: &Connection, delegation: &DelegationRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO delegation_log (
            brain_session, worker_session, task, agent,
            requested_at, completed_at, status, diff_stats
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            delegation.brain_session,
            delegation.worker_session,
            delegation.task,
            delegation.agent,
            delegation.requested_at,
            delegation.completed_at,
            delegation.status,
            delegation.diff_stats,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update a delegation when it completes.
pub fn update_delegation_end(
    conn: &Connection,
    id: i64,
    status: &str,
    diff_stats: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE delegation_log SET completed_at = ?1, status = ?2, diff_stats = ?3 WHERE id = ?4",
        params![now, status, diff_stats, id],
    )?;
    Ok(())
}

// ─── Cost Aggregation Queries ─────────────────────────────────────────

/// Sum costs by agent for today (UTC).
pub fn query_cost_today(conn: &Connection) -> Result<Vec<CostSummary>> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT agent,
                COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
                COUNT(*) AS session_count,
                COALESCE(SUM(duration_seconds), 0) AS total_duration
         FROM sessions
         WHERE started_at >= ?1
         GROUP BY agent
         ORDER BY total_cost DESC",
    )?;

    let rows = stmt.query_map(params![today], |row| {
        Ok(CostSummary {
            agent: row.get(0)?,
            total_cost_usd: row.get(1)?,
            session_count: row.get(2)?,
            total_duration_seconds: row.get(3)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Sum costs by agent for a date range (ISO 8601 strings, inclusive).
pub fn query_cost_range(conn: &Connection, from: &str, to: &str) -> Result<Vec<CostSummary>> {
    let mut stmt = conn.prepare(
        "SELECT agent,
                COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
                COUNT(*) AS session_count,
                COALESCE(SUM(duration_seconds), 0) AS total_duration
         FROM sessions
         WHERE started_at >= ?1 AND started_at < ?2
         GROUP BY agent
         ORDER BY total_cost DESC",
    )?;

    let rows = stmt.query_map(params![from, to], |row| {
        Ok(CostSummary {
            agent: row.get(0)?,
            total_cost_usd: row.get(1)?,
            session_count: row.get(2)?,
            total_duration_seconds: row.get(3)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Sum costs grouped by project.
pub fn query_cost_by_project(conn: &Connection) -> Result<Vec<ProjectCostSummary>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(project, '(unassigned)') AS proj,
                COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
                COUNT(*) AS session_count
         FROM sessions
         GROUP BY proj
         ORDER BY total_cost DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ProjectCostSummary {
            project: row.get(0)?,
            total_cost_usd: row.get(1)?,
            session_count: row.get(2)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
