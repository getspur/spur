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
    // Token-based fields (nullable for backward compatibility)
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
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

/// Token summary grouped by agent.
#[derive(Debug, Clone)]
pub struct TokenSummary {
    pub agent: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub session_count: i64,
}

// ─── Database Initialization ──────────────────────────────────────────

/// Open (or create) the SQLite database at `path` and ensure the schema exists.
pub fn init_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

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
            estimated_cost_usd REAL,
            model TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            cache_creation_tokens INTEGER,
            cache_read_tokens INTEGER
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
        
        CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent);
        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project);
        CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at);
        
        CREATE INDEX IF NOT EXISTS idx_delegation_brain ON delegation_log(brain_session);
        CREATE INDEX IF NOT EXISTS idx_delegation_worker ON delegation_log(worker_session);
        
        -- Migration guard: these columns are now in CREATE TABLE above.
        -- If upgrading from a schema prior to v0.4.5, run:
        --   ALTER TABLE sessions ADD COLUMN model TEXT;
        --   ALTER TABLE sessions ADD COLUMN input_tokens INTEGER;
        --   ALTER TABLE sessions ADD COLUMN output_tokens INTEGER;
        --   ALTER TABLE sessions ADD COLUMN cache_creation_tokens INTEGER;
        --   ALTER TABLE sessions ADD COLUMN cache_read_tokens INTEGER;
        
        -- Create view for token summary
        CREATE VIEW IF NOT EXISTS v_session_tokens AS
        SELECT
            agent,
            COALESCE(SUM(input_tokens), 0) AS total_input_tokens,
            COALESCE(SUM(output_tokens), 0) AS total_output_tokens,
            COALESCE(SUM(cache_creation_tokens), 0) AS total_cache_creation_tokens,
            COALESCE(SUM(cache_read_tokens), 0) AS total_cache_read_tokens,
            COUNT(*) AS session_count
        FROM sessions
        WHERE input_tokens IS NOT NULL OR output_tokens IS NOT NULL
        GROUP BY agent;
        
        CREATE VIEW IF NOT EXISTS v_model_costs AS
        SELECT
            model,
            agent,
            COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
            COUNT(*) AS session_count,
            COALESCE(SUM(duration_seconds), 0) AS total_duration
        FROM sessions
        WHERE model IS NOT NULL
        GROUP BY model, agent;
        
        CREATE VIEW IF NOT EXISTS v_daily_costs AS
        SELECT
            date(started_at) AS day,
            agent,
            COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
            COUNT(*) AS session_count,
            COALESCE(SUM(duration_seconds), 0) AS total_duration
        FROM sessions
        GROUP BY date(started_at), agent;
        
        CREATE VIEW IF NOT EXISTS v_daily_tokens AS
        SELECT
            date(started_at) AS day,
            agent,
            COALESCE(SUM(input_tokens), 0) AS total_input_tokens,
            COALESCE(SUM(output_tokens), 0) AS total_output_tokens,
            COALESCE(SUM(cache_creation_tokens), 0) AS total_cache_creation_tokens,
            COALESCE(SUM(cache_read_tokens), 0) AS total_cache_read_tokens,
            COUNT(*) AS session_count
        FROM sessions
        WHERE input_tokens IS NOT NULL OR output_tokens IS NOT NULL
        GROUP BY date(started_at), agent;
        
        -- Ensure backward compatibility by creating a view for v1 schema access
        CREATE VIEW IF NOT EXISTS v_session_costs AS
        SELECT
            id,
            agent,
            role,
            parent_session,
            task_summary,
            project,
            issue_ref,
            started_at,
            ended_at,
            status,
            duration_seconds,
            estimated_cost_usd
        FROM sessions;
        
        -- Create index on model column for cost analysis queries
        CREATE INDEX IF NOT EXISTS idx_sessions_model ON sessions(model);
        ",
    )?;

    Ok(conn)
}

// ─── Row Helpers ─────────────────────────────────────────────────────

fn session_from_row(row: &rusqlite::Row) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
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
        model: row.get(12)?,
        input_tokens: row.get(13)?,
        output_tokens: row.get(14)?,
        cache_creation_tokens: row.get(15)?,
        cache_read_tokens: row.get(16)?,
    })
}

// ─── Session Queries ──────────────────────────────────────────────────

/// Insert a new session record.
pub fn insert_session(conn: &Connection, session: &SessionRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (
            id, agent, role, parent_session, task_summary, project,
            issue_ref, started_at, ended_at, status, duration_seconds,
            estimated_cost_usd, model, input_tokens, output_tokens,
            cache_creation_tokens, cache_read_tokens
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
            session.model,
            session.input_tokens,
            session.output_tokens,
            session.cache_creation_tokens,
            session.cache_read_tokens,
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

/// Update a session when it ends, including token counts and model.
#[allow(clippy::too_many_arguments)]
pub fn update_session_end_with_tokens(
    conn: &Connection,
    id: &str,
    status: &str,
    duration: i64,
    cost: f64,
    model: Option<&str>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE sessions SET
            ended_at = ?1,
            status = ?2,
            duration_seconds = ?3,
            estimated_cost_usd = ?4,
            model = ?5,
            input_tokens = ?6,
            output_tokens = ?7,
            cache_creation_tokens = ?8,
            cache_read_tokens = ?9
         WHERE id = ?10",
        params![
            now,
            status,
            duration,
            cost,
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            id,
        ],
    )?;
    Ok(())
}

/// Retrieve a single session and attach its delegations (as brain or worker).
pub fn query_session(conn: &Connection, id: &str) -> Result<Option<SessionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent, role, parent_session, task_summary, project,
                issue_ref, started_at, ended_at, status, duration_seconds,
                estimated_cost_usd, model, input_tokens, output_tokens,
                cache_creation_tokens, cache_read_tokens
         FROM sessions WHERE id = ?1",
    )?;

    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(session_from_row(row)?)),
        None => Ok(None),
    }
}

/// Return the most recent sessions, ordered by started_at descending.
pub fn query_recent_sessions(conn: &Connection, limit: usize) -> Result<Vec<SessionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent, role, parent_session, task_summary, project,
                issue_ref, started_at, ended_at, status, duration_seconds,
                estimated_cost_usd, model, input_tokens, output_tokens,
                cache_creation_tokens, cache_read_tokens
         FROM sessions
         ORDER BY started_at DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![limit as i64], session_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Return all delegations where the given session is either the brain or the worker.
pub fn query_delegations_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<DelegationRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, brain_session, worker_session, task, agent,
                requested_at, completed_at, status, diff_stats
         FROM delegation_log
         WHERE brain_session = ?1 OR worker_session = ?1
         ORDER BY requested_at ASC",
    )?;

    let rows = stmt.query_map(params![session_id], |row| {
        Ok(DelegationRecord {
            id: row.get(0)?,
            brain_session: row.get(1)?,
            worker_session: row.get(2)?,
            task: row.get(3)?,
            agent: row.get(4)?,
            requested_at: row.get(5)?,
            completed_at: row.get(6)?,
            status: row.get(7)?,
            diff_stats: row.get(8)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

// ─── Token Aggregation Queries ────────────────────────────────────────

/// Sum token usage by agent for today (UTC).
pub fn query_tokens_today(conn: &Connection) -> Result<Vec<TokenSummary>> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT agent,
                COALESCE(SUM(input_tokens), 0) AS total_input,
                COALESCE(SUM(output_tokens), 0) AS total_output,
                COALESCE(SUM(cache_creation_tokens), 0) AS total_cache_creation,
                COALESCE(SUM(cache_read_tokens), 0) AS total_cache_read,
                COUNT(*) AS session_count
         FROM sessions
         WHERE started_at >= ?1
           AND (input_tokens IS NOT NULL OR output_tokens IS NOT NULL)
         GROUP BY agent
         ORDER BY total_input + total_output DESC",
    )?;

    let rows = stmt.query_map(params![today], |row| {
        Ok(TokenSummary {
            agent: row.get(0)?,
            total_input_tokens: row.get(1)?,
            total_output_tokens: row.get(2)?,
            total_cache_creation_tokens: row.get(3)?,
            total_cache_read_tokens: row.get(4)?,
            session_count: row.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Sum token usage by agent for a date range (ISO 8601 strings, inclusive).
pub fn query_tokens_range(conn: &Connection, from: &str, to: &str) -> Result<Vec<TokenSummary>> {
    let mut stmt = conn.prepare(
        "SELECT agent,
                COALESCE(SUM(input_tokens), 0) AS total_input,
                COALESCE(SUM(output_tokens), 0) AS total_output,
                COALESCE(SUM(cache_creation_tokens), 0) AS total_cache_creation,
                COALESCE(SUM(cache_read_tokens), 0) AS total_cache_read,
                COUNT(*) AS session_count
         FROM sessions
         WHERE started_at >= ?1 AND started_at < ?2
           AND (input_tokens IS NOT NULL OR output_tokens IS NOT NULL)
         GROUP BY agent
         ORDER BY total_input + total_output DESC",
    )?;

    let rows = stmt.query_map(params![from, to], |row| {
        Ok(TokenSummary {
            agent: row.get(0)?,
            total_input_tokens: row.get(1)?,
            total_output_tokens: row.get(2)?,
            total_cache_creation_tokens: row.get(3)?,
            total_cache_read_tokens: row.get(4)?,
            session_count: row.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Cost summary grouped by model and agent.
#[derive(Debug, Clone)]
pub struct ModelCostSummary {
    pub model: String,
    pub agent: String,
    pub total_cost_usd: f64,
    pub session_count: i64,
    pub total_duration_seconds: i64,
}

/// Sum costs grouped by model.
pub fn query_cost_by_model(conn: &Connection) -> Result<Vec<ModelCostSummary>> {
    let mut stmt = conn.prepare(
        "SELECT model, agent,
                COALESCE(SUM(estimated_cost_usd), 0.0) AS total_cost,
                COUNT(*) AS session_count,
                COALESCE(SUM(duration_seconds), 0) AS total_duration
         FROM sessions
         WHERE model IS NOT NULL
         GROUP BY model, agent
         ORDER BY total_cost DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ModelCostSummary {
            model: row.get(0)?,
            agent: row.get(1)?,
            total_cost_usd: row.get(2)?,
            session_count: row.get(3)?,
            total_duration_seconds: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
