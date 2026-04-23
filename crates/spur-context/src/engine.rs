//! DuckDB analytics engine for SPUR.
//!
//! The engine connects to DuckDB, initializes SQL convert views for each agent,
//! loads pricing data, and exposes typed query methods for reports and live mode.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use duckdb::{params, Connection};
use std::env;
use std::path::{Path, PathBuf};
use tracing;

// ─── SQL Definitions ──────────────────────────────────────────────────

const SCHEMA_SQL: &str = include_str!("sql/schema.sql");

// ─── Engine ───────────────────────────────────────────────────────────

/// DuckDB-backed analytics engine.
///
/// Reads agent JSONL files in place via SQL convert views.
/// No intermediate files, no data migration.
///
/// # Performance Note
///
/// `duckdb::Connection` maintains an internal LRU statement cache, so
/// repeated `prepare()` calls for the same SQL are cheap (cache hit).
/// There is no need for an additional prepared-statement caching layer
/// in this struct.
pub struct AnalyticsEngine {
    conn: Connection,
}

impl AnalyticsEngine {
    /// Open a persistent DuckDB database.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path).context("failed to open DuckDB")?;
        tracing::debug!("opened DuckDB analytics engine");
        Ok(Self { conn })
    }

    /// Open an in-memory DuckDB (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
        tracing::debug!("opened in-memory DuckDB analytics engine");
        Ok(Self { conn })
    }

    /// Initialize base schema: pricing table + placeholder views.
    ///
    /// Call `create_agent_views()` after this to set up per-agent
    /// convert views based on discovered data directories.
    pub fn initialize(&self) -> Result<()> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .context("failed to initialize DuckDB base schema")?;
        tracing::debug!("initialized DuckDB base schema");
        Ok(())
    }

    /// Create agent convert views by discovering agent data directories.
    ///
    /// For each agent, checks whether its data directory exists.
    /// If yes, creates a `read_json_auto()` view. If no, creates
    /// an empty stub view so `all_events` UNION ALL still works.
    pub fn create_agent_views(&self) -> Result<AgentViewStatus> {
        let mut status = AgentViewStatus::default();

        // ─── Claude ─────────────────────────────────────────────
        let claude_dir = Self::discover_claude_dir();
        if Self::has_jsonl_files(&claude_dir) {
            match self.create_claude_view(&claude_dir) {
                Ok(()) => {
                    status.claude = true;
                    tracing::debug!(dir = %claude_dir.display(), "created claude_events view");
                }
                Err(e) => {
                    tracing::warn!(dir = %claude_dir.display(), error = %e, "failed to create claude_events view, using stub");
                    self.create_empty_stub("claude_events")?;
                }
            }
        } else {
            self.create_empty_stub("claude_events")?;
            tracing::debug!("created empty claude_events stub");
        }

        // ─── Codex ──────────────────────────────────────────────
        let codex_dir = Self::discover_codex_dir();
        if Self::has_jsonl_files(&codex_dir) {
            match self.create_codex_view(&codex_dir) {
                Ok(()) => {
                    status.codex = true;
                    tracing::debug!(dir = %codex_dir.display(), "created codex_events view");
                }
                Err(e) => {
                    tracing::warn!(dir = %codex_dir.display(), error = %e, "failed to create codex_events view, using stub");
                    self.create_empty_stub("codex_events")?;
                }
            }
        } else {
            self.create_empty_stub("codex_events")?;
            tracing::debug!("created empty codex_events stub");
        }

        // ─── Kiro ───────────────────────────────────────────────
        let kiro_dir = Self::discover_kiro_dir();
        if Self::has_jsonl_files(&kiro_dir) {
            match self.create_kiro_view(&kiro_dir) {
                Ok(()) => {
                    status.kiro = true;
                    tracing::debug!(dir = %kiro_dir.display(), "created kiro_events view");
                }
                Err(e) => {
                    tracing::warn!(dir = %kiro_dir.display(), error = %e, "failed to create kiro_events view, using stub");
                    self.create_empty_stub("kiro_events")?;
                }
            }
        } else {
            self.create_empty_stub("kiro_events")?;
            tracing::debug!("created empty kiro_events stub");
        }

        // ─── Rebuild unified views ─────────────────────────────
        self.rebuild_unified_views()?;

        Ok(status)
    }

    fn discover_claude_dir() -> PathBuf {
        env::var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                directories::BaseDirs::new()
                    .map(|b| b.home_dir().join(".config/claude/projects"))
                    .unwrap_or_else(|| PathBuf::from("~/.config/claude/projects"))
            })
    }

    fn discover_codex_dir() -> PathBuf {
        env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                directories::BaseDirs::new()
                    .map(|b| b.home_dir().join(".codex/sessions"))
                    .unwrap_or_else(|| PathBuf::from("~/.codex/sessions"))
            })
    }

    fn discover_kiro_dir() -> PathBuf {
        env::var("KIRO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                directories::BaseDirs::new()
                    .map(|b| b.home_dir().join(".kiro/sessions"))
                    .unwrap_or_else(|| PathBuf::from("~/.kiro/sessions"))
            })
    }

    fn has_jsonl_files(dir: &Path) -> bool {
        if !dir.is_dir() {
            return false;
        }
        Self::find_jsonl_files(dir).is_ok_and(|v| !v.is_empty())
    }

    fn find_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(Self::find_jsonl_files(&path)?);
            } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn create_claude_view(&self, dir: &Path) -> Result<()> {
        let pattern = format!(
            "{}/**/*.jsonl",
            dir.to_string_lossy().replace('\\', "/").replace('\'', "''")
        );
        let sql = format!(
            r#"CREATE OR REPLACE VIEW claude_events AS
             SELECT
                timestamp::TIMESTAMP AS timestamp,
                sessionId AS session_id,
                'claude' AS agent,
                NULLIF(message.model, '<synthetic>') AS model,
                NULLIF(regexp_extract(filename, '.*/projects/([^/]+)/.*[.]jsonl$', 1), '') AS project,
                message.usage.input_tokens AS input_tokens,
                message.usage.output_tokens AS output_tokens,
                message.usage.cache_read_input_tokens AS cache_read_tokens,
                message.usage.cache_creation_input_tokens AS cache_creation_tokens,
                costUSD AS cost_usd
             FROM read_json_auto('{}', filename = true, ignore_errors = true)"#,
            pattern
        );
        self.conn
            .execute_batch(&sql)
            .with_context(|| format!("failed to create claude_events view for {}", pattern))?;
        Ok(())
    }

    fn create_codex_view(&self, dir: &Path) -> Result<()> {
        let pattern = format!(
            "{}/**/*.jsonl",
            dir.to_string_lossy().replace('\\', "/").replace('\'', "''")
        );
        let sql = format!(
            "CREATE OR REPLACE VIEW codex_events AS \
             WITH raw AS ( \
                SELECT \
                    timestamp::TIMESTAMP AS ts, \
                    session_id, \
                    model, \
                    last_token_usage.input_tokens AS last_input, \
                    last_token_usage.output_tokens AS last_output, \
                    last_token_usage.cached_input_tokens AS last_cached, \
                    total_token_usage.input_tokens AS total_input, \
                    total_token_usage.output_tokens AS total_output, \
                    total_token_usage.cached_input_tokens AS total_cached \
                FROM read_json_auto('{}', ignore_errors = true) \
             ), \
             with_lag AS ( \
                SELECT *, \
                    LAG(total_input) OVER (PARTITION BY session_id ORDER BY ts) AS prev_input, \
                    LAG(total_output) OVER (PARTITION BY session_id ORDER BY ts) AS prev_output, \
                    LAG(total_cached) OVER (PARTITION BY session_id ORDER BY ts) AS prev_cached \
                FROM raw \
             ) \
             SELECT \
                ts AS timestamp, \
                session_id, \
                'codex' AS agent, \
                COALESCE(NULLIF(model, ''), 'gpt-5') AS model, \
                NULL::VARCHAR AS project, \
                COALESCE(last_input, total_input - COALESCE(prev_input, 0)) AS input_tokens, \
                COALESCE(last_output, total_output - COALESCE(prev_output, 0)) AS output_tokens, \
                LEAST( \
                    COALESCE(last_cached, total_cached - COALESCE(prev_cached, 0)), \
                    COALESCE(last_input, total_input - COALESCE(prev_input, 0)) \
                ) AS cache_read_tokens, \
                0::BIGINT AS cache_creation_tokens, \
                NULL::DOUBLE AS cost_usd \
             FROM with_lag",
            pattern
        );
        self.conn
            .execute_batch(&sql)
            .with_context(|| format!("failed to create codex_events view for {}", pattern))?;
        Ok(())
    }

    fn create_kiro_view(&self, _dir: &Path) -> Result<()> {
        // Kiro format is not yet documented; create empty stub
        self.create_empty_stub("kiro_events")
    }

    fn create_empty_stub(&self, view_name: &str) -> Result<()> {
        let sql = format!(
            "CREATE OR REPLACE VIEW {} AS \
             SELECT \
                NULL::TIMESTAMP AS timestamp, \
                NULL::VARCHAR AS session_id, \
                '{}' AS agent, \
                NULL::VARCHAR AS model, \
                NULL::VARCHAR AS project, \
                0::BIGINT AS input_tokens, \
                0::BIGINT AS output_tokens, \
                0::BIGINT AS cache_read_tokens, \
                0::BIGINT AS cache_creation_tokens, \
                NULL::DOUBLE AS cost_usd \
             WHERE FALSE",
            view_name,
            view_name.trim_end_matches("_events")
        );
        self.conn
            .execute_batch(&sql)
            .with_context(|| format!("failed to create empty stub for {}", view_name))?;
        Ok(())
    }

    fn rebuild_unified_views(&self) -> Result<()> {
        let sql = r#"
            CREATE OR REPLACE VIEW all_events AS
            SELECT * FROM claude_events
            UNION ALL
            SELECT * FROM codex_events
            UNION ALL
            SELECT * FROM kiro_events;

            CREATE OR REPLACE VIEW all_events_with_cost AS
            SELECT
                e.*,
                COALESCE(
                    e.cost_usd,
                    (e.input_tokens * p.input_price_per_1m / 1000000.0)
                    + (e.output_tokens * p.output_price_per_1m / 1000000.0)
                    + (e.cache_read_tokens * p.cache_read_price_per_1m / 1000000.0)
                    + (e.cache_creation_tokens * p.cache_creation_price_per_1m / 1000000.0)
                ) AS computed_cost_usd
            FROM all_events e
            LEFT JOIN pricing p
                ON lower(e.model) = lower(p.model)
                AND e.timestamp >= p.effective_from
                AND (p.effective_to IS NULL OR e.timestamp < p.effective_to);
        "#;
        self.conn
            .execute_batch(sql)
            .context("failed to rebuild unified views")?;
        Ok(())
    }

    /// Load pricing data from the Rust PricingRegistry into DuckDB.
    pub fn load_pricing(&self, registry: &spur_cost::PricingRegistry) -> Result<()> {
        // Clear existing pricing
        self.conn
            .execute("DELETE FROM pricing", [])
            .context("failed to clear pricing table")?;

        let mut stmt = self.conn.prepare(
            "INSERT INTO pricing (model, input_price_per_1m, output_price_per_1m,
             cache_read_price_per_1m, cache_creation_price_per_1m)
             VALUES (?, ?, ?, ?, ?)",
        )?;

        for model in registry.known_models() {
            if let Some(pricing) = registry.get(&model) {
                stmt.execute(params![
                    model,
                    pricing.input_cost_per_token * 1_000_000.0,
                    pricing.output_cost_per_token * 1_000_000.0,
                    pricing.cache_read_input_token_cost * 1_000_000.0,
                    pricing.cache_creation_input_token_cost * 1_000_000.0,
                ])
                .with_context(|| format!("failed to insert pricing for model {}", model))?;
            }
        }

        // Also insert aliases as separate rows pointing to same prices
        // (simpler than handling alias resolution in SQL)
        self.insert_alias_pricing(registry)?;

        tracing::debug!("loaded pricing into DuckDB");
        Ok(())
    }

    fn insert_alias_pricing(&self, registry: &spur_cost::PricingRegistry) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO pricing (model, input_price_per_1m, output_price_per_1m,
             cache_read_price_per_1m, cache_creation_price_per_1m)
             VALUES (?, ?, ?, ?, ?)",
        )?;

        for (alias, canonical) in registry.aliases() {
            let Some(pricing) = registry.get(&canonical) else {
                tracing::warn!(
                    alias = %alias,
                    canonical = %canonical,
                    "alias points to unknown canonical model; skipping"
                );
                continue;
            };
            stmt.execute(params![
                alias,
                pricing.input_cost_per_token * 1_000_000.0,
                pricing.output_cost_per_token * 1_000_000.0,
                pricing.cache_read_input_token_cost * 1_000_000.0,
                pricing.cache_creation_input_token_cost * 1_000_000.0,
            ])
            .with_context(|| format!("failed to insert alias pricing for {}", alias))?;
        }

        Ok(())
    }

    // ─── Report Queries ───────────────────────────────────────────────

    /// Daily cost report for the last N days.
    pub fn daily_report(&self, days: u32) -> Result<Vec<DailyRow>> {
        let sql = r#"
            SELECT
                strftime(timestamp, '%Y-%m-%d') AS day,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= current_date - CAST(? || ' days' AS INTERVAL)
            GROUP BY day, agent
            ORDER BY day DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([days], |row| {
            Ok(DailyRow {
                day: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("daily report query failed")
    }

    /// Weekly cost report for the last N weeks.
    pub fn weekly_report(&self, weeks: u32) -> Result<Vec<WeeklyRow>> {
        let sql = r#"
            SELECT
                strftime(date_trunc('week', timestamp), '%Y-%m-%d') AS week,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= current_date - CAST(? || ' weeks' AS INTERVAL)
            GROUP BY week, agent
            ORDER BY week DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([weeks], |row| {
            Ok(WeeklyRow {
                week: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("weekly report query failed")
    }

    /// Monthly cost report for the last N months.
    pub fn monthly_report(&self, months: u32) -> Result<Vec<MonthlyRow>> {
        let sql = r#"
            SELECT
                strftime(timestamp, '%Y-%m') AS month,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= current_date - CAST(? || ' months' AS INTERVAL)
            GROUP BY month, agent
            ORDER BY month DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([months], |row| {
            Ok(MonthlyRow {
                month: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("monthly report query failed")
    }

    /// Daily cost report for a specific date range.
    pub fn daily_report_range(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<DailyRow>> {
        let sql = r#"
            SELECT
                strftime(timestamp, '%Y-%m-%d') AS day,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= CAST(? AS DATE) AND timestamp < CAST(? AS DATE)
            GROUP BY day, agent
            ORDER BY day DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let start_str = start.format("%Y-%m-%d").to_string();
        let end_str = end.format("%Y-%m-%d").to_string();
        let rows = stmt.query_map([&start_str, &end_str], |row| {
            Ok(DailyRow {
                day: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("daily report range query failed")
    }

    /// Weekly cost report for a specific date range.
    pub fn weekly_report_range(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<WeeklyRow>> {
        let sql = r#"
            SELECT
                strftime(date_trunc('week', timestamp), '%Y-%m-%d') AS week,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= CAST(? AS DATE) AND timestamp < CAST(? AS DATE)
            GROUP BY week, agent
            ORDER BY week DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let start_str = start.format("%Y-%m-%d").to_string();
        let end_str = end.format("%Y-%m-%d").to_string();
        let rows = stmt.query_map([&start_str, &end_str], |row| {
            Ok(WeeklyRow {
                week: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("weekly report range query failed")
    }

    /// Monthly cost report for a specific date range.
    pub fn monthly_report_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<MonthlyRow>> {
        let sql = r#"
            SELECT
                strftime(timestamp, '%Y-%m') AS month,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            WHERE timestamp >= CAST(? AS DATE) AND timestamp < CAST(? AS DATE)
            GROUP BY month, agent
            ORDER BY month DESC, cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let start_str = start.format("%Y-%m-%d").to_string();
        let end_str = end.format("%Y-%m-%d").to_string();
        let rows = stmt.query_map([&start_str, &end_str], |row| {
            Ok(MonthlyRow {
                month: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_creation_tokens: row.get(6)?,
                cost_usd: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("monthly report range query failed")
    }

    /// Live recent sessions within the last N minutes.
    pub fn live_recent_sessions(&self, minutes: u32) -> Result<Vec<LiveBlockRow>> {
        let sql = r#"
            SELECT
                session_id,
                agent,
                model,
                strftime(MIN(timestamp), '%Y-%m-%dT%H:%M:%S') AS started_at,
                strftime(MAX(timestamp), '%Y-%m-%dT%H:%M:%S') AS last_activity,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
                COUNT(*) AS events
            FROM all_events_with_cost
            WHERE timestamp >= now() - CAST(? || ' minutes' AS INTERVAL)
            GROUP BY session_id, agent, model
            ORDER BY cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([minutes], |row| {
            Ok(LiveBlockRow {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                last_activity: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_creation_tokens: row.get(8)?,
                cost_usd: row.get(9)?,
                events: row.get(10)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("live recent sessions query failed")
    }

    /// Model cost breakdown.
    pub fn model_breakdown(&self) -> Result<Vec<ModelRow>> {
        let sql = r#"
            SELECT
                model,
                agent,
                COUNT(*) AS requests,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                ROUND(AVG(computed_cost_usd), 6) AS avg_cost,
                ROUND(SUM(computed_cost_usd), 4) AS total_cost
            FROM all_events_with_cost
            WHERE model IS NOT NULL
            GROUP BY model, agent
            ORDER BY total_cost DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(ModelRow {
                model: row.get(0)?,
                agent: row.get(1)?,
                requests: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                avg_cost: row.get(5)?,
                total_cost: row.get(6)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("model breakdown query failed")
    }

    /// Project cost breakdown.
    pub fn project_breakdown(&self) -> Result<Vec<ProjectRow>> {
        let sql = r#"
            SELECT
                COALESCE(project, '(none)') AS project,
                agent,
                COUNT(DISTINCT session_id) AS sessions,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd
            FROM all_events_with_cost
            GROUP BY project, agent
            ORDER BY cost_usd DESC
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(ProjectRow {
                project: row.get(0)?,
                agent: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cost_usd: row.get(5)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("project breakdown query failed")
    }

    /// Detail for a single session.
    pub fn session_detail(&self, session_id: &str) -> Result<Option<SessionRow>> {
        let sql = r#"
            SELECT
                session_id,
                agent,
                model,
                strftime(MIN(timestamp), '%Y-%m-%dT%H:%M:%S') AS started_at,
                strftime(MAX(timestamp), '%Y-%m-%dT%H:%M:%S') AS ended_at,
                EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp)))::BIGINT AS duration_seconds,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
                COUNT(*) AS events
            FROM all_events_with_cost
            WHERE session_id = ?
            GROUP BY session_id, agent, model
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map([session_id], |row| {
            Ok(SessionRow {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                duration_seconds: row.get(5)?,
                input_tokens: row.get(6)?,
                output_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_creation_tokens: row.get(9)?,
                cost_usd: row.get(10)?,
                events: row.get(11)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Live snapshot for an active session.
    ///
    /// This is optimized for frequent polling (every 1-5 seconds).
    pub fn live_session_snapshot(&self, session_id: &str) -> Result<Option<LiveSnapshot>> {
        let sql = r#"
            SELECT
                session_id,
                agent,
                model,
                strftime(MIN(timestamp), '%Y-%m-%dT%H:%M:%S') AS started_at,
                strftime(MAX(timestamp), '%Y-%m-%dT%H:%M:%S') AS last_activity,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
                COUNT(*) AS events
            FROM all_events_with_cost
            WHERE session_id = ?
            GROUP BY session_id, agent, model
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map([session_id], |row| {
            Ok(LiveSnapshot {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                last_activity: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_creation_tokens: row.get(8)?,
                cost_usd: row.get(9)?,
                events: row.get(10)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Execute a raw SQL query and return results as JSON strings.
    ///
    /// Useful for ad-hoc analytics and MCP tool integration.
    pub fn query_json(&self, sql: &str) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(sql)?;
        let column_count = stmt.column_count();
        let column_names: Vec<String> = (0..column_count)
            .map(|i| {
                stmt.column_name(i)
                    .map_or("?".to_string(), |s| s.to_string())
            })
            .collect();

        let rows = stmt.query_map([], |row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in column_names.iter().enumerate() {
                let value: String = row.get(i).unwrap_or_default();
                obj.insert(name.clone(), serde_json::Value::String(value));
            }
            Ok(serde_json::Value::Object(obj))
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("raw JSON query failed")
    }

    /// Get the underlying DuckDB connection (for advanced use).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

// ─── Row Types ────────────────────────────────────────────────────────

/// Daily cost report row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyRow {
    pub day: String,
    pub agent: String,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
}

/// Weekly cost report row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WeeklyRow {
    pub week: String,
    pub agent: String,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
}

/// Monthly cost report row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MonthlyRow {
    pub month: String,
    pub agent: String,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
}

/// Model breakdown row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelRow {
    pub model: String,
    pub agent: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub avg_cost: f64,
    pub total_cost: f64,
}

/// Project breakdown row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectRow {
    pub project: String,
    pub agent: String,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

/// Session detail row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub events: i64,
}

/// Status of agent view creation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentViewStatus {
    pub claude: bool,
    pub codex: bool,
    pub kiro: bool,
}

/// Live session block row for recent activity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveBlockRow {
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub last_activity: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub events: i64,
}

/// Live session snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveSnapshot {
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub last_activity: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub events: i64,
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_engine() -> AnalyticsEngine {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();
        engine
    }

    #[test]
    fn test_schema_initialization() {
        let engine = setup_engine();
        // Verify pricing table exists
        let count: i64 = engine
            .conn
            .query_row("SELECT COUNT(*) FROM pricing", [], |r| r.get(0))
            .unwrap();
        assert!(count > 0, "pricing table should have rows");
    }

    #[test]
    fn test_claude_events_from_fixture() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude/projects/spur");
        std::fs::create_dir_all(&claude_dir).unwrap();

        // Write a Claude-style JSONL file
        let jsonl_path = claude_dir.join("2026-04-23.jsonl");
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        writeln!(file, r#"{{"timestamp":"2026-04-23T10:00:00Z","sessionId":"sess-1","message":{{"usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":100,"cache_read_input_tokens":200}},"model":"claude-sonnet-4","id":"msg_1"}},"costUSD":0.05,"requestId":"req_1"}}"#).unwrap();
        writeln!(file, r#"{{"timestamp":"2026-04-23T10:01:00Z","sessionId":"sess-1","message":{{"usage":{{"input_tokens":2000,"output_tokens":1000,"cache_creation_input_tokens":150,"cache_read_input_tokens":400}},"model":"claude-sonnet-4","id":"msg_2"}},"costUSD":0.10,"requestId":"req_2"}}"#).unwrap();

        // Override the view to read from temp dir
        let engine = setup_engine();
        engine
            .conn
            .execute_batch(&format!(
                r#"CREATE OR REPLACE VIEW claude_events AS
             SELECT
                timestamp::TIMESTAMP AS timestamp,
                sessionId AS session_id,
                'claude' AS agent,
                NULLIF(message.model, '<synthetic>') AS model,
                NULLIF(regexp_extract(filename, '.*/projects/([^/]+)/.*[.]jsonl$', 1), '') AS project,
                message.usage.input_tokens AS input_tokens,
                message.usage.output_tokens AS output_tokens,
                message.usage.cache_read_input_tokens AS cache_read_tokens,
                message.usage.cache_creation_input_tokens AS cache_creation_tokens,
                costUSD AS cost_usd
             FROM read_json_auto('{}', filename = true, ignore_errors = true)"#,
                jsonl_path.to_str().unwrap().replace('\\', "/")
            ))
            .unwrap();

        // Query the unified view
        let count: i64 = engine
            .conn
            .query_row("SELECT COUNT(*) FROM claude_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "should have 2 claude events");

        let (project, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens): (
            String,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
        ) = engine
            .conn
            .query_row(
                "SELECT project, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens \
                 FROM claude_events ORDER BY timestamp LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(project, "spur");
        assert_eq!(model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(input_tokens, 1000);
        assert_eq!(output_tokens, 500);
        assert_eq!(cache_read_tokens, 200);
        assert_eq!(cache_creation_tokens, 100);

        // Query cost-enriched view
        let cost: f64 = engine
            .conn
            .query_row(
                "SELECT SUM(computed_cost_usd) FROM all_events_with_cost WHERE agent = 'claude'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (cost - 0.15).abs() < 0.001,
            "cost should be 0.15, got {}",
            cost
        );
    }

    #[test]
    fn test_codex_events_delta_logic() {
        let tmp = TempDir::new().unwrap();
        let codex_dir = tmp.path().join("codex/sessions/spur");
        std::fs::create_dir_all(&codex_dir).unwrap();

        // Write a Codex-style JSONL file with cumulative totals
        let jsonl_path = codex_dir.join("session.jsonl");
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        // Row 1: first entry, no previous
        writeln!(file, r#"{{"timestamp":"2026-04-23T10:00:00Z","session_id":"sess-codex-1","total_token_usage":{{"input_tokens":100,"output_tokens":50,"cached_input_tokens":10}},"model":"gpt-5"}}"#).unwrap();
        // Row 2: second entry, cumulative
        writeln!(file, r#"{{"timestamp":"2026-04-23T10:01:00Z","session_id":"sess-codex-1","total_token_usage":{{"input_tokens":300,"output_tokens":150,"cached_input_tokens":30}},"model":"gpt-5"}}"#).unwrap();
        // Row 3: third entry with last_token_usage (direct delta)
        writeln!(file, r#"{{"timestamp":"2026-04-23T10:02:00Z","session_id":"sess-codex-1","last_token_usage":{{"input_tokens":50,"output_tokens":25,"cached_input_tokens":5}},"total_token_usage":{{"input_tokens":350,"output_tokens":175,"cached_input_tokens":35}},"model":"gpt-5"}}"#).unwrap();

        let engine = setup_engine();
        engine
            .conn
            .execute_batch(&format!(
                "CREATE OR REPLACE VIEW codex_events AS \
             WITH raw AS ( \
                SELECT \
                    timestamp::TIMESTAMP AS ts, \
                    session_id, \
                    model, \
                    last_token_usage.input_tokens AS last_input, \
                    last_token_usage.output_tokens AS last_output, \
                    last_token_usage.cached_input_tokens AS last_cached, \
                    total_token_usage.input_tokens AS total_input, \
                    total_token_usage.output_tokens AS total_output, \
                    total_token_usage.cached_input_tokens AS total_cached, \
                FROM read_json_auto('{}', ignore_errors=true) \
             ), \
             with_lag AS ( \
                SELECT *, \
                    LAG(total_input) OVER (PARTITION BY session_id ORDER BY ts) AS prev_input, \
                    LAG(total_output) OVER (PARTITION BY session_id ORDER BY ts) AS prev_output, \
                    LAG(total_cached) OVER (PARTITION BY session_id ORDER BY ts) AS prev_cached \
                FROM raw \
             ) \
             SELECT \
                ts AS timestamp, \
                session_id, \
                'codex' AS agent, \
                COALESCE(NULLIF(model, ''), 'gpt-5') AS model, \
                NULL::VARCHAR AS project, \
                COALESCE(last_input, total_input - COALESCE(prev_input, 0)) AS input_tokens, \
                COALESCE(last_output, total_output - COALESCE(prev_output, 0)) AS output_tokens, \
                LEAST( \
                    COALESCE(last_cached, total_cached - COALESCE(prev_cached, 0)), \
                    COALESCE(last_input, total_input - COALESCE(prev_input, 0)) \
                ) AS cache_read_tokens, \
                0::BIGINT AS cache_creation_tokens, \
                NULL::DOUBLE AS cost_usd \
             FROM with_lag",
                jsonl_path.to_str().unwrap().replace('\\', "/")
            ))
            .unwrap();

        // Verify delta computation
        let mut stmt = engine.conn.prepare(
            "SELECT input_tokens, output_tokens, cache_read_tokens FROM codex_events ORDER BY timestamp"
        ).unwrap();
        let rows: Vec<(i64, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows.len(), 3);
        // Row 1: total - 0 (no prev) = 100, 50, 10
        assert_eq!(rows[0], (100, 50, 10));
        // Row 2: total - prev = 300-100=200, 150-50=100, 30-10=20
        assert_eq!(rows[1], (200, 100, 20));
        // Row 3: last_token_usage = 50, 25, 5 (and capped at input)
        assert_eq!(rows[2], (50, 25, 5));
    }

    #[test]
    fn test_daily_report_empty() {
        let engine = setup_engine();
        let report = engine.daily_report(7).unwrap();
        assert!(
            report.is_empty(),
            "daily report should be empty with no data"
        );
    }

    #[test]
    fn test_model_breakdown_empty() {
        let engine = setup_engine();
        let report = engine.model_breakdown().unwrap();
        assert!(
            report.is_empty(),
            "model breakdown should be empty with no data"
        );
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────
