//! DuckDB analytics engine for SPUR.
//!
//! The engine connects to DuckDB, initializes SQL convert views for each agent,
//! loads pricing data, and exposes typed query methods for reports and live mode.

#[cfg(feature = "duckdb")]
use anyhow::Context;
use anyhow::Result;
use chrono::NaiveDate;
#[cfg(feature = "duckdb")]
use chrono::Utc;
#[cfg(feature = "duckdb")]
use duckdb::{params, Connection};
#[cfg(feature = "duckdb")]
use std::env;
use std::path::Path;
#[cfg(feature = "duckdb")]
use std::path::PathBuf;
#[cfg(feature = "duckdb")]
use tracing;

// ─── SQL Definitions ──────────────────────────────────────────────────

#[cfg(feature = "duckdb")]
const SCHEMA_SQL: &str = include_str!("sql/schema.sql");
#[cfg(feature = "duckdb")]
const DAILY_REPORT_SQL: &str = include_str!("sql/daily_report.sql");
#[cfg(feature = "duckdb")]
const WEEKLY_REPORT_SQL: &str = include_str!("sql/weekly_report.sql");
#[cfg(feature = "duckdb")]
const MONTHLY_REPORT_SQL: &str = include_str!("sql/monthly_report.sql");
#[cfg(feature = "duckdb")]
const MODEL_BREAKDOWN_SQL: &str = include_str!("sql/model_breakdown.sql");
#[cfg(feature = "duckdb")]
const PROJECT_BREAKDOWN_SQL: &str = include_str!("sql/project_breakdown.sql");
#[cfg(feature = "duckdb")]
const SESSION_DETAIL_SQL: &str = include_str!("sql/session_detail.sql");
#[cfg(feature = "duckdb")]
const LIVE_SNAPSHOT_SQL: &str = include_str!("sql/live_session_snapshot.sql");

/// Cost-enrichment view.
///
/// For each event we find the **longest** pricing row whose canonical
/// model name either equals `e.model` case-insensitively or is a prefix
/// followed by a `-` or `.` boundary. That mirrors the dash-or-dot
/// boundary used by `PricingRegistry::get()` in Rust, so the two paths
/// agree on which pricing row applies to e.g. `gpt-5.4` → `gpt-5`,
/// `claude-opus-4-6` → `claude-opus-4`, without requiring every
/// post-cutoff variant to be registered as an alias.
///
/// The pricing table is small (O(100) rows), so the correlated LATERAL
/// scan is effectively free; DuckDB runs it once per distinct model.
#[cfg(feature = "duckdb")]
const ALL_EVENTS_WITH_COST_VIEW: &str = r#"
    CREATE OR REPLACE VIEW all_events_with_cost AS
    SELECT
        e.*,
        CASE
            WHEN e.cost_usd IS NOT NULL THEN 'native'
            WHEN p.model IS NOT NULL THEN 'priced'
            ELSE 'unpriced'
        END AS cost_source,
        COALESCE(
            e.cost_usd,
            (e.input_tokens * p.input_price_per_1m / 1000000.0)
            + (e.output_tokens * p.output_price_per_1m / 1000000.0)
            + (e.cache_read_tokens * p.cache_read_price_per_1m / 1000000.0)
            + (e.cache_creation_tokens * p.cache_creation_price_per_1m / 1000000.0)
        ) AS computed_cost_usd
    FROM all_events e
    LEFT JOIN LATERAL (
        SELECT pp.*
        FROM pricing pp
        WHERE e.model IS NOT NULL
          AND (
              lower(e.model) = lower(pp.model)
              OR lower(e.model) LIKE lower(pp.model) || '-%'
              OR lower(e.model) LIKE lower(pp.model) || '.%'
          )
          AND e.timestamp >= pp.effective_from
          AND (pp.effective_to IS NULL OR e.timestamp < pp.effective_to)
        ORDER BY length(pp.model) DESC, pp.model ASC
        LIMIT 1
    ) p ON TRUE;
"#;

/// Strip a leading `<segment>/` from a model id.
///
/// OpenCode stores model strings as `<provider>/<canonical>`. The pricing
/// registry keys on the canonical name, so strip the provider prefix at
/// extraction time. Only the first slash is consumed.
#[cfg(feature = "duckdb")]
fn strip_provider_prefix(s: &str) -> &str {
    s.split_once('/').map_or(s, |(_, rest)| rest)
}

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
#[cfg(feature = "duckdb")]
pub struct AnalyticsEngine {
    conn: Connection,
}

#[cfg(feature = "duckdb")]
impl AnalyticsEngine {
    /// Open a persistent DuckDB database.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<(Self, bool)> {
        let path = path.as_ref();
        let conn = match Self::open_connection(path) {
            Ok(conn) => conn,
            Err(original_error) => {
                if !format!("{original_error:#}").contains("Corrupt WAL") {
                    return Err(original_error);
                }
                Self::move_corrupt_wal_aside(path)?;
                match Self::open_connection(path) {
                    Ok(conn) => {
                        tracing::warn!(
                            path = %path.display(),
                            "recovered DuckDB analytics engine after corrupt WAL"
                        );
                        tracing::debug!("opened DuckDB analytics engine");
                        return Ok((Self { conn }, true));
                    }
                    Err(_) => return Err(original_error),
                }
            }
        };
        tracing::debug!("opened DuckDB analytics engine");
        Ok((Self { conn }, false))
    }

    /// Open an in-memory DuckDB (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
        tracing::debug!("opened in-memory DuckDB analytics engine");
        Ok(Self { conn })
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        Connection::open(path).context("failed to open DuckDB")
    }

    fn move_corrupt_wal_aside(path: &Path) -> Result<()> {
        let wal_path = wal_path_for(path);
        let broken_path = broken_wal_path_for(&wal_path);
        // Tolerate races where a parallel SPUR process already moved the
        // WAL: that instance will own the recovery; we fall through and
        // let DuckDB surface its normal lock error on retry-open.
        if let Err(error) = std::fs::rename(&wal_path, &broken_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(anyhow::Error::new(error).context(format!(
                    "failed to rename corrupt DuckDB WAL {} to {}",
                    wal_path.display(),
                    broken_path.display()
                )));
            }
        }
        if let Err(error) = gc_broken_wals(path) {
            tracing::warn!(
                error = %format!("{error:#}"),
                "gc_broken_wals failed; continuing with recovery"
            );
        }
        Ok(())
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

    // ─── Phase 2.5: persistent cache / incremental refresh ─────────────

    /// Refresh `events_cache` if any agent JSONL file is newer than the
    /// last-recorded load time. Returns the number of rows materialized
    /// (zero if the cache was up-to-date).
    ///
    /// Staleness detection is coarse: if the newest mtime across the
    /// discovered directories exceeds any agent's `loaded_at` in
    /// `scan_manifest`, the entire cache is rebuilt. This keeps the
    /// implementation simple; per-file incremental refresh can be added
    /// as a future refinement.
    pub fn refresh_cache(&self) -> Result<i64> {
        use std::time::SystemTime;
        let max_mtime = Self::newest_agent_mtime();
        let loaded_at: Option<SystemTime> = self
            .conn
            .query_row(
                "SELECT epoch(MIN(loaded_at))::DOUBLE FROM scan_manifest",
                [],
                |r| {
                    r.get::<_, Option<f64>>(0).map(|v| {
                        v.map(|secs| {
                            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs_f64(secs)
                        })
                    })
                },
            )
            .unwrap_or(None);
        let fresh = match (max_mtime, loaded_at) {
            (Some(m), Some(l)) => m <= l,
            (None, Some(_)) => true, // no files to load
            _ => false,              // never loaded or no manifest yet
        };
        if fresh {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM events_cache", [], |r| r.get(0))
                .unwrap_or(0);
            tracing::debug!("events_cache is fresh ({} rows)", count);
            return Ok(0);
        }

        // Stale — TRUNCATE + INSERT. Source from `all_events_raw` (the
        // stable UNION), never `all_events` (which `use_cached_events`
        // may have rebound to `events_cache` itself, producing a
        // self-wipe).
        self.conn.execute_batch(
            r#"
            DELETE FROM events_cache;
            INSERT INTO events_cache
              SELECT timestamp, session_id, agent, model, project,
                     input_tokens, output_tokens, cache_read_tokens,
                     cache_creation_tokens, cost_usd
              FROM all_events_raw;
            DELETE FROM scan_manifest;
            "#,
        )?;
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events_cache", [], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO scan_manifest (agent, loaded_at, row_count)
             SELECT agent, now(), COUNT(*) FROM events_cache GROUP BY agent",
            [],
        )?;
        tracing::debug!("materialized {} rows into events_cache", total);
        let checkpoint_started = std::time::Instant::now();
        self.checkpoint()?;
        let elapsed = checkpoint_started.elapsed();
        tracing::debug!("refresh_cache: post-INSERT checkpoint took {elapsed:?}");
        Ok(total)
    }

    /// Force DuckDB to flush pending WAL contents into the database file.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("CHECKPOINT;")
            .context("failed to checkpoint DuckDB")?;
        Ok(())
    }

    /// Point `all_events` (and thus `all_events_with_cost`) at the
    /// materialized `events_cache` table. Call after `refresh_cache()`
    /// to make subsequent report queries sub-second.
    ///
    /// `all_events_raw` is **not** affected — `refresh_cache` keeps
    /// reading the raw UNION through that stable name regardless of how
    /// often `use_cached_events` is called.
    pub fn use_cached_events(&self) -> Result<()> {
        self.conn
            .execute_batch("CREATE OR REPLACE VIEW all_events AS SELECT * FROM events_cache;")?;
        // Rebuild all_events_with_cost so its underlying reference
        // resolves to the cache-backed view. The longest-prefix lateral
        // matcher is unchanged — only the backing view differs.
        self.conn.execute_batch(ALL_EVENTS_WITH_COST_VIEW)?;
        tracing::debug!("all_events view now backed by events_cache table");
        Ok(())
    }

    fn newest_agent_mtime() -> Option<std::time::SystemTime> {
        let mut newest: Option<std::time::SystemTime> = None;
        {
            let mut bump = |m| {
                newest = Some(match newest {
                    Some(cur) if cur >= m => cur,
                    _ => m,
                });
            };

            for dir in [
                Self::discover_claude_dir(),
                Self::discover_codex_dir(),
                Self::discover_kiro_dir(),
                Self::discover_kimi_dir(),
            ] {
                if let Ok(files) = Self::find_jsonl_files(&dir) {
                    for f in files {
                        if let Ok(meta) = std::fs::metadata(&f) {
                            if let Ok(m) = meta.modified() {
                                bump(m);
                            }
                        }
                    }
                }
            }

            // Gemini uses JSON documents, not JSONL logs.
            if let Ok(files) = Self::find_files_with_ext(&Self::discover_gemini_dir(), "json") {
                for f in files {
                    if let Ok(meta) = std::fs::metadata(&f) {
                        if let Ok(m) = meta.modified() {
                            bump(m);
                        }
                    }
                }
            }

            // OpenCode is a single SQLite file, not a directory walk.
            let opencode_db = Self::discover_opencode_db();
            if opencode_db.is_file() {
                if let Ok(meta) = std::fs::metadata(&opencode_db) {
                    if let Ok(m) = meta.modified() {
                        bump(m);
                    }
                }
            }
        }
        newest
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

        // ─── OpenCode ───────────────────────────────────────────
        let opencode_db = Self::discover_opencode_db();
        if Self::has_opencode_db(&opencode_db) {
            match self.create_opencode_view(&opencode_db) {
                Ok(()) => {
                    status.opencode = true;
                    tracing::debug!(db = %opencode_db.display(), "created opencode_events view");
                }
                Err(e) => {
                    tracing::warn!(db = %opencode_db.display(), error = %e, "failed to create opencode_events view, using stub");
                    self.create_empty_stub("opencode_events")?;
                }
            }
        } else {
            self.create_empty_stub("opencode_events")?;
            tracing::debug!("created empty opencode_events stub");
        }

        // ─── Kimi ───────────────────────────────────────────────
        let kimi_dir = Self::discover_kimi_dir();
        if kimi_dir.is_dir() {
            match self.create_kimi_view(&kimi_dir) {
                Ok(()) => {
                    status.kimi = true;
                    tracing::debug!(dir = %kimi_dir.display(), "created kimi_events view");
                }
                Err(e) => {
                    tracing::warn!(dir = %kimi_dir.display(), error = %e, "failed to create kimi_events view, using stub");
                    self.create_empty_stub("kimi_events")?;
                }
            }
        } else {
            self.create_empty_stub("kimi_events")?;
            tracing::debug!("created empty kimi_events stub");
        }

        // ─── Gemini ─────────────────────────────────────────────
        let gemini_dir = Self::discover_gemini_dir();
        if gemini_dir.is_dir() {
            match self.create_gemini_view(&gemini_dir) {
                Ok(()) => {
                    status.gemini = true;
                    tracing::debug!(dir = %gemini_dir.display(), "created gemini_events view");
                }
                Err(e) => {
                    tracing::warn!(dir = %gemini_dir.display(), error = %e, "failed to create gemini_events view, using stub");
                    self.create_empty_stub("gemini_events")?;
                }
            }
        } else {
            self.create_empty_stub("gemini_events")?;
            tracing::debug!("created empty gemini_events stub");
        }

        // ─── Rebuild unified views ─────────────────────────────
        self.rebuild_unified_views()?;

        Ok(status)
    }

    fn discover_claude_dir() -> PathBuf {
        // Probe in order: $CLAUDE_CONFIG_DIR → ~/.claude/projects → ~/.config/claude/projects.
        // First existing directory wins. Real Claude Code writes to ~/.claude/projects;
        // the XDG-style path is an older convention kept as a last-resort fallback.
        if let Ok(path) = env::var("CLAUDE_CONFIG_DIR") {
            let p = PathBuf::from(path);
            if p.is_dir() {
                return p;
            }
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/claude")
        }
        #[cfg(not(test))]
        {
            let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
            if let Some(h) = home.as_ref() {
                let native = h.join(".claude/projects");
                if native.is_dir() {
                    return native;
                }
                let xdg = h.join(".config/claude/projects");
                if xdg.is_dir() {
                    return xdg;
                }
                // Nothing exists — return the native-convention path so downstream
                // has_jsonl_files() returns false and a stub view is created.
                return native;
            }
            PathBuf::from("~/.claude/projects")
        }
    }

    fn discover_codex_dir() -> PathBuf {
        if let Ok(path) = env::var("CODEX_HOME") {
            return PathBuf::from(path);
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/codex")
        }
        #[cfg(not(test))]
        {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".codex/sessions"))
                .unwrap_or_else(|| PathBuf::from("~/.codex/sessions"))
        }
    }

    fn discover_kiro_dir() -> PathBuf {
        if let Ok(path) = env::var("KIRO_HOME") {
            return PathBuf::from(path);
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/kiro")
        }
        #[cfg(not(test))]
        {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".kiro/sessions"))
                .unwrap_or_else(|| PathBuf::from("~/.kiro/sessions"))
        }
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

    fn find_files_with_ext(dir: &Path, ext: &str) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        if !dir.is_dir() {
            return Ok(files);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(Self::find_files_with_ext(&path, ext)?);
            } else if path.extension().and_then(|s| s.to_str()) == Some(ext) {
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
            r#"CREATE OR REPLACE VIEW claude_raw AS
             SELECT
                rtrim(line, chr(13)) AS line,
                filename
             FROM read_csv_auto(
                '{}',
                columns = {{'line': 'VARCHAR'}},
                delim = '\0',
                header = false,
                filename = true,
                ignore_errors = true,
                quote = '',
                escape = ''
             );

             CREATE OR REPLACE VIEW claude_events AS
             SELECT
                timestamp, session_id, agent, model, project,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd
             FROM (
                SELECT
                    TRY_CAST(json_extract_string(line, '$.timestamp') AS TIMESTAMP) AS timestamp,
                    json_extract_string(line, '$.sessionId') AS session_id,
                    'claude' AS agent,
                    NULLIF(json_extract_string(line, '$.message.model'), '<synthetic>') AS model,
                    NULLIF(regexp_extract(filename, '.*/projects/([^/]+)/.*[.]jsonl$', 1), '') AS project,
                    TRY_CAST(json_extract(line, '$.message.usage.input_tokens') AS BIGINT) AS input_tokens,
                    TRY_CAST(json_extract(line, '$.message.usage.output_tokens') AS BIGINT) AS output_tokens,
                    TRY_CAST(json_extract(line, '$.message.usage.cache_read_input_tokens') AS BIGINT) AS cache_read_tokens,
                    TRY_CAST(json_extract(line, '$.message.usage.cache_creation_input_tokens') AS BIGINT) AS cache_creation_tokens,
                    TRY_CAST(json_extract(line, '$.costUSD') AS DOUBLE) AS cost_usd,
                    ROW_NUMBER() OVER (
                        PARTITION BY
                            json_extract_string(line, '$.sessionId'),
                            COALESCE(json_extract_string(line, '$.requestId'), ''),
                            COALESCE(json_extract_string(line, '$.message.id'), '')
                        ORDER BY TRY_CAST(json_extract_string(line, '$.timestamp') AS TIMESTAMP)
                    ) AS _dedup_rn
                FROM claude_raw
                WHERE json_valid(line)
                  AND json_extract_string(line, '$.type') = 'assistant'
             )
             WHERE _dedup_rn = 1;"#,
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
            r#"CREATE OR REPLACE VIEW codex_raw AS
             SELECT
                rtrim(line, chr(13)) AS line,
                filename
             FROM read_csv_auto(
                '{}',
                columns = {{'line': 'VARCHAR'}},
                delim = '\0',
                header = false,
                filename = true,
                ignore_errors = true,
                quote = '',
                escape = ''
             );

             CREATE OR REPLACE VIEW codex_token_events AS
             SELECT
                TRY_CAST(json_extract_string(line, '$.timestamp') AS TIMESTAMP) AS ts,
                NULLIF(regexp_extract(filename, '.*/([^/]+)[.]jsonl$', 1), '') AS session_id,
                json_extract_string(line, '$.type') AS type,
                json_extract_string(line, '$.payload.type') AS payload_type,
                json_extract_string(line, '$.payload.model') AS turn_model,
                json_extract_string(line, '$.payload.info.model') AS event_model,
                TRY_CAST(json_extract(line, '$.payload.info.last_token_usage.input_tokens') AS BIGINT) AS last_in,
                TRY_CAST(json_extract(line, '$.payload.info.last_token_usage.output_tokens') AS BIGINT) AS last_out,
                TRY_CAST(json_extract(line, '$.payload.info.last_token_usage.cached_input_tokens') AS BIGINT) AS last_cached,
                TRY_CAST(json_extract(line, '$.payload.info.total_token_usage.input_tokens') AS BIGINT) AS tot_in,
                TRY_CAST(json_extract(line, '$.payload.info.total_token_usage.output_tokens') AS BIGINT) AS tot_out,
                TRY_CAST(json_extract(line, '$.payload.info.total_token_usage.cached_input_tokens') AS BIGINT) AS tot_cached,
                filename
             FROM codex_raw
             WHERE json_valid(line);

             CREATE OR REPLACE VIEW codex_events AS
             WITH with_carried_model AS (
                SELECT
                    *,
                    LAST_VALUE(COALESCE(event_model, turn_model) IGNORE NULLS) OVER (
                        PARTITION BY filename ORDER BY ts
                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                    ) AS current_model
                FROM codex_token_events
                WHERE ts IS NOT NULL
             ),
             with_delta AS (
                SELECT
                    *,
                    COALESCE(
                        last_in,
                        GREATEST(
                            tot_in - COALESCE(LAG(tot_in) OVER (PARTITION BY session_id ORDER BY ts), 0),
                            0::BIGINT
                        )
                    ) AS input_delta,
                    COALESCE(
                        last_out,
                        GREATEST(
                            tot_out - COALESCE(LAG(tot_out) OVER (PARTITION BY session_id ORDER BY ts), 0),
                            0::BIGINT
                        )
                    ) AS output_delta,
                    COALESCE(
                        last_cached,
                        GREATEST(
                            tot_cached - COALESCE(LAG(tot_cached) OVER (PARTITION BY session_id ORDER BY ts), 0),
                            0::BIGINT
                        )
                    ) AS cached_delta
                FROM with_carried_model
                WHERE type = 'event_msg' AND payload_type = 'token_count'
             )
             SELECT
                ts AS timestamp,
                session_id,
                'codex' AS agent,
                NULLIF(NULLIF(current_model, ''), '<synthetic>') AS model,
                NULL::VARCHAR AS project,
                -- P0.4: Codex reports input_tokens as a SUPERSET of cached_input_tokens
                -- (verified 2026-04-24 against 13,499 live token_count rows:
                -- sum_in + sum_out = sum_total exactly, so cached ⊂ input). Subtract
                -- the cached portion so `input_tokens` means billable non-cached input
                -- consistently across Claude and Codex.
                GREATEST(input_delta - LEAST(cached_delta, input_delta), 0::BIGINT) AS input_tokens,
                output_delta AS output_tokens,
                LEAST(cached_delta, input_delta) AS cache_read_tokens,
                0::BIGINT AS cache_creation_tokens,
                NULL::DOUBLE AS cost_usd
             FROM with_delta
             WHERE input_delta > 0 OR output_delta > 0 OR cached_delta > 0;"#,
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

    /// Discover the OpenCode SQLite database path.
    ///
    /// Probe order: `$OPENCODE_DATA_DIR/opencode.db` → XDG `~/.local/share/opencode/opencode.db`.
    /// Returns the path even if the file doesn't exist so `has_opencode_db()` can
    /// discriminate; callers must check existence before opening.
    fn discover_opencode_db() -> PathBuf {
        if let Ok(path) = env::var("OPENCODE_DATA_DIR") {
            let p = PathBuf::from(path);
            // Allow either a directory containing opencode.db or the db path itself.
            if p.is_file() {
                return p;
            }
            return p.join("opencode.db");
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/opencode.db")
        }
        #[cfg(not(test))]
        {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".local/share/opencode/opencode.db"))
                .unwrap_or_else(|| PathBuf::from("~/.local/share/opencode/opencode.db"))
        }
    }

    /// Discover the Kimi sessions directory.
    ///
    /// Probe order: `$KIMI_HOME/sessions` → `~/.kimi/sessions`.
    fn discover_kimi_dir() -> PathBuf {
        if let Ok(path) = env::var("KIMI_HOME") {
            return PathBuf::from(path).join("sessions");
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/kimi")
        }
        #[cfg(not(test))]
        {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".kimi/sessions"))
                .unwrap_or_else(|| PathBuf::from("~/.kimi/sessions"))
        }
    }

    /// Discover the Gemini sessions directory.
    ///
    /// Probe order: `$GEMINI_HOME/tmp` → `~/.gemini/tmp`.
    fn discover_gemini_dir() -> PathBuf {
        if let Ok(path) = env::var("GEMINI_HOME") {
            return PathBuf::from(path).join("tmp");
        }
        #[cfg(test)]
        {
            PathBuf::from("__spur_context_test_missing__/gemini")
        }
        #[cfg(not(test))]
        {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".gemini/tmp"))
                .unwrap_or_else(|| PathBuf::from("~/.gemini/tmp"))
        }
    }

    fn has_opencode_db(path: &Path) -> bool {
        path.is_file()
    }

    /// Populate `opencode_events` by extracting assistant messages from the
    /// OpenCode SQLite database.
    ///
    /// Unlike JSONL-based agents, OpenCode stores sessions in SQLite (Drizzle
    /// ORM). We use `rusqlite` to read rows and push them into a DuckDB
    /// materialized table via the appender API, then expose it as a view.
    /// Cost is passed through verbatim — OpenCode computes it from the
    /// upstream provider's `usage` block per response and that value is
    /// authoritative; we do NOT re-apply pricing.
    ///
    /// Rows with all-zero token fields (typically failed API calls) are
    /// skipped to match the semantics of `codex_events`.
    fn create_opencode_view(&self, db_path: &Path) -> Result<()> {
        // Fresh materialized table — drop any prior contents so re-runs are
        // idempotent and the view reflects current DB state.
        // Underlying table stores timestamp as BIGINT milliseconds-since-epoch
        // because the DuckDB appender doesn't accept a raw i64 into a TIMESTAMP
        // column. The exposed view casts it back to a proper TIMESTAMP so the
        // UNION in `all_events` stays type-compatible.
        self.conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS opencode_events_table;
            CREATE TABLE opencode_events_table (
                timestamp_ms          BIGINT,
                session_id            VARCHAR,
                agent                 VARCHAR,
                model                 VARCHAR,
                project               VARCHAR,
                input_tokens          BIGINT,
                output_tokens         BIGINT,
                cache_read_tokens     BIGINT,
                cache_creation_tokens BIGINT,
                cost_usd              DOUBLE
            );
            "#,
        )?;

        let rows = Self::extract_opencode_rows(db_path)
            .with_context(|| format!("failed to read opencode db at {}", db_path.display()))?;

        if !rows.is_empty() {
            let mut appender = self
                .conn
                .appender("opencode_events_table")
                .context("failed to open opencode_events_table appender")?;
            for r in &rows {
                appender
                    .append_row(params![
                        r.timestamp_ms,
                        r.session_id,
                        "opencode",
                        r.model,
                        r.project,
                        r.input_tokens,
                        r.output_tokens,
                        r.cache_read_tokens,
                        r.cache_creation_tokens,
                        r.cost_usd,
                    ])
                    .context("failed to append opencode row")?;
            }
            appender
                .flush()
                .context("failed to flush opencode appender")?;
        }

        // Expose as a view with an explicit TIMESTAMP cast so the UNION in
        // `all_events` is type-compatible with the other agents.
        self.conn.execute_batch(
            r#"
            CREATE OR REPLACE VIEW opencode_events AS
            SELECT
                epoch_ms(timestamp_ms) AS timestamp,
                session_id,
                agent,
                model,
                project,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                cost_usd
            FROM opencode_events_table;
            "#,
        )?;

        tracing::debug!(
            path = %db_path.display(),
            rows = rows.len(),
            "populated opencode_events"
        );
        Ok(())
    }

    /// Populate `kimi_events` from Kimi session JSONL files.
    ///
    /// `sessions_root` is the `.kimi/sessions` directory. We walk
    /// `<project_hash>/<session_uuid>/context.jsonl`, pair odd/even `_usage`
    /// rows per turn, and emit one event per assistant turn. File mtime is
    /// used as the base timestamp with a back-dating offset so intra-session
    /// ordering is preserved.
    fn create_kimi_view(&self, sessions_root: &Path) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS kimi_events_table;
            CREATE TABLE kimi_events_table (
                timestamp_ms          BIGINT,
                session_id            VARCHAR,
                agent                 VARCHAR,
                model                 VARCHAR,
                project               VARCHAR,
                input_tokens          BIGINT,
                output_tokens         BIGINT,
                cache_read_tokens     BIGINT,
                cache_creation_tokens BIGINT,
                cost_usd              DOUBLE
            );
            "#,
        )?;

        let rows = Self::extract_kimi_rows(sessions_root).with_context(|| {
            format!(
                "failed to scan kimi sessions at {}",
                sessions_root.display()
            )
        })?;

        if !rows.is_empty() {
            let mut appender = self
                .conn
                .appender("kimi_events_table")
                .context("failed to open kimi_events_table appender")?;
            for r in &rows {
                appender
                    .append_row(params![
                        r.timestamp_ms,
                        r.session_id,
                        "kimi",
                        "kimi-for-coding",
                        r.project,
                        r.input_tokens,
                        r.output_tokens,
                        0_i64,
                        0_i64,
                        None::<f64>,
                    ])
                    .context("failed to append kimi row")?;
            }
            appender.flush().context("failed to flush kimi appender")?;
        }

        self.conn.execute_batch(
            r#"
            CREATE OR REPLACE VIEW kimi_events AS
            SELECT
                epoch_ms(timestamp_ms) AS timestamp,
                session_id,
                agent,
                model,
                project,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                cost_usd
            FROM kimi_events_table;
            "#,
        )?;

        tracing::debug!(
            root = %sessions_root.display(),
            rows = rows.len(),
            "populated kimi_events"
        );
        Ok(())
    }

    /// Populate `gemini_events` from Gemini CLI session JSON files.
    fn create_gemini_view(&self, tmp_root: &Path) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS gemini_events_table;
            CREATE TABLE gemini_events_table (
                timestamp_ms          BIGINT,
                session_id            VARCHAR,
                agent                 VARCHAR,
                model                 VARCHAR,
                project               VARCHAR,
                input_tokens          BIGINT,
                output_tokens         BIGINT,
                cache_read_tokens     BIGINT,
                cache_creation_tokens BIGINT,
                cost_usd              DOUBLE
            );
            "#,
        )?;

        let rows = crate::extractors::gemini::extract(tmp_root).with_context(|| {
            format!(
                "failed to extract gemini sessions at {}",
                tmp_root.display()
            )
        })?;

        if !rows.is_empty() {
            let mut appender = self
                .conn
                .appender("gemini_events_table")
                .context("failed to open gemini_events_table appender")?;
            for r in &rows {
                appender
                    .append_row(params![
                        r.timestamp.timestamp_millis(),
                        r.session_id,
                        "gemini",
                        r.model,
                        r.project,
                        r.input_tokens,
                        r.output_tokens,
                        r.cache_read_tokens,
                        r.cache_creation_tokens,
                        r.cost_usd,
                    ])
                    .context("failed to append gemini row")?;
            }
            appender
                .flush()
                .context("failed to flush gemini appender")?;
        }

        self.conn.execute_batch(
            r#"
            CREATE OR REPLACE VIEW gemini_events AS
            SELECT
                epoch_ms(timestamp_ms) AS timestamp,
                session_id,
                agent,
                model,
                project,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                cost_usd
            FROM gemini_events_table;
            "#,
        )?;
        Ok(())
    }

    fn extract_kimi_rows(sessions_root: &Path) -> Result<Vec<KimiRow>> {
        use std::io::BufRead;

        let mut out = Vec::new();
        if !sessions_root.is_dir() {
            return Ok(out);
        }
        for project_entry in std::fs::read_dir(sessions_root)? {
            let project_entry = project_entry?;
            if !project_entry.path().is_dir() {
                continue;
            }
            let project = project_entry.file_name().to_string_lossy().to_string();
            for session_entry in std::fs::read_dir(project_entry.path())? {
                let session_entry = session_entry?;
                let session_dir = session_entry.path();
                let ctx = session_dir.join("context.jsonl");
                if !ctx.is_file() {
                    continue;
                }
                let session_id = session_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let mtime_ms = std::fs::metadata(&ctx)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);

                let file = std::fs::File::open(&ctx)
                    .with_context(|| format!("failed to open {}", ctx.display()))?;
                let mut usage: Vec<u64> = Vec::new();
                let mut assistant_turns: usize = 0;
                for line in std::io::BufReader::new(file).lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(row) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    match role {
                        "_usage" => {
                            if let Some(t) = row.get("token_count").and_then(|v| v.as_u64()) {
                                usage.push(t);
                            }
                        }
                        "assistant" => assistant_turns += 1,
                        _ => {}
                    }
                }

                let pair_ok = usage.len() == assistant_turns * 2 && !usage.is_empty();
                if pair_ok {
                    let turns = usage.len() / 2;
                    let mut prev_post: Option<u64> = None;
                    for t in 0..turns {
                        let pre = usage[t * 2];
                        let post = usage[t * 2 + 1];
                        let output = post.saturating_sub(pre);
                        let input = match prev_post {
                            Some(p) => pre.saturating_sub(p),
                            None => pre,
                        };
                        prev_post = Some(post);
                        if output == 0 && input == 0 {
                            continue;
                        }
                        let ts_offset_ms = ((turns - t - 1) as i64) * 1000;
                        out.push(KimiRow {
                            timestamp_ms: mtime_ms.saturating_sub(ts_offset_ms),
                            session_id: session_id.clone(),
                            project: Some(project.clone()),
                            input_tokens: input as i64,
                            output_tokens: output as i64,
                        });
                    }
                } else {
                    // Fallback: cumulative diffs as input tokens only. Still useful volume.
                    tracing::warn!(
                        file = %ctx.display(),
                        usage_count = usage.len(),
                        assistant_count = assistant_turns,
                        "kimi _usage/assistant count mismatch; falling back to input-only deltas"
                    );
                    let mut prev = 0u64;
                    let total = usage.len();
                    for (i, &cur) in usage.iter().enumerate() {
                        let delta = cur.saturating_sub(prev);
                        prev = cur;
                        if delta == 0 {
                            continue;
                        }
                        let ts_offset_ms = ((total - i - 1) as i64) * 1000;
                        out.push(KimiRow {
                            timestamp_ms: mtime_ms.saturating_sub(ts_offset_ms),
                            session_id: session_id.clone(),
                            project: Some(project.clone()),
                            input_tokens: delta as i64,
                            output_tokens: 0,
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    fn extract_opencode_rows(db_path: &Path) -> Result<Vec<OpenCodeRow>> {
        // `mode=ro` + `immutable=1` avoids creating WAL sidecar files and is
        // safe even if the user's opencode process has the DB open for writes.
        let uri = format!("file:{}?mode=ro&immutable=1", db_path.to_string_lossy());
        let conn = rusqlite::Connection::open_with_flags(
            uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;

        let mut stmt = conn.prepare(
            r#"
            SELECT m.time_created, m.session_id, p.worktree, m.data
            FROM message m
            JOIN session s ON s.id = m.session_id
            JOIN project p ON p.id = s.project_id
            WHERE json_extract(m.data, '$.role') = 'assistant'
            "#,
        )?;

        let raw: Vec<(i64, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = Vec::with_capacity(raw.len());
        for (ts_ms, session_id, worktree, data_json) in raw {
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&data_json) else {
                continue;
            };
            let tokens = data
                .get("tokens")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let input = tokens.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
            let output = tokens.get("output").and_then(|v| v.as_i64()).unwrap_or(0);
            let reasoning = tokens
                .get("reasoning")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let cache_read = tokens
                .get("cache")
                .and_then(|c| c.get("read"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let cache_write = tokens
                .get("cache")
                .and_then(|c| c.get("write"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            // Skip all-zero rows — typically failed API calls with an error
            // payload and no real usage.
            if input == 0 && output == 0 && reasoning == 0 && cache_read == 0 && cache_write == 0 {
                continue;
            }

            let model = data
                .get("modelID")
                .and_then(|v| v.as_str())
                .map(|s| strip_provider_prefix(s).to_string());
            let cost = data.get("cost").and_then(|v| v.as_f64());
            let project = std::path::Path::new(&worktree)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());

            out.push(OpenCodeRow {
                timestamp_ms: ts_ms,
                session_id,
                model,
                project,
                input_tokens: input,
                // Reasoning tokens are billed as output by OpenRouter / most
                // upstream providers; fold them in to match that accounting.
                output_tokens: output + reasoning,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_write,
                cost_usd: cost,
            });
        }
        Ok(out)
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

    /// Build the unified event views.
    ///
    /// Two views with distinct lifecycles:
    /// - `all_events_raw` is the **stable** UNION over per-agent views.
    ///   `refresh_cache` always sources its INSERT from this view.
    /// - `all_events` is the **mutable** query-facing view. Initially
    ///   it aliases `all_events_raw`; after `use_cached_events()` it is
    ///   replaced to alias `events_cache` so report queries hit the
    ///   materialized cache instead of re-scanning JSONL.
    ///
    /// Keeping the two names separate avoids the self-wipe bug where
    /// `refresh_cache` previously read from `all_events` after that
    /// name had been rebound to `events_cache`.
    fn rebuild_unified_views(&self) -> Result<()> {
        let sql = format!(
            r#"
            CREATE OR REPLACE VIEW all_events_raw AS
            SELECT * FROM claude_events
            UNION ALL
            SELECT * FROM codex_events
            UNION ALL
            SELECT * FROM kiro_events
            UNION ALL
            SELECT * FROM opencode_events
            UNION ALL
            SELECT * FROM kimi_events
            UNION ALL
            SELECT * FROM gemini_events;

            CREATE OR REPLACE VIEW all_events AS SELECT * FROM all_events_raw;

            {}
            "#,
            ALL_EVENTS_WITH_COST_VIEW
        );
        self.conn
            .execute_batch(&sql)
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
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd
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
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd
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
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd
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
        let mut stmt = self.conn.prepare(DAILY_REPORT_SQL)?;
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
        let mut stmt = self.conn.prepare(WEEKLY_REPORT_SQL)?;
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
        let mut stmt = self.conn.prepare(MONTHLY_REPORT_SQL)?;
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
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd,
                COUNT(*) AS events
            FROM all_events_with_cost
            WHERE timestamp >= (now() AT TIME ZONE 'UTC') - CAST(? || ' minutes' AS INTERVAL)
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
        let mut stmt = self.conn.prepare(MODEL_BREAKDOWN_SQL)?;
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
        let mut stmt = self.conn.prepare(PROJECT_BREAKDOWN_SQL)?;
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
        let mut stmt = self.conn.prepare(SESSION_DETAIL_SQL)?;
        let mut rows = stmt.query_map([session_id], |row| {
            Ok(SessionRow {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                models: row.get(2)?,
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
        let mut stmt = self.conn.prepare(LIVE_SNAPSHOT_SQL)?;
        let mut rows = stmt.query_map([session_id], |row| {
            Ok(LiveSnapshot {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                models: row.get(2)?,
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
    #[cfg(feature = "duckdb")]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(feature = "duckdb")]
fn wal_path_for(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_os_string();
    wal.push(".wal");
    PathBuf::from(wal)
}

#[cfg(feature = "duckdb")]
fn broken_wal_path_for(wal_path: &Path) -> PathBuf {
    let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
    let mut broken = wal_path.as_os_str().to_os_string();
    broken.push(format!(".{timestamp}.broken"));
    PathBuf::from(broken)
}

#[cfg(feature = "duckdb")]
fn gc_broken_wals(db_path: &Path) -> Result<()> {
    let Some(db_name) = db_path.file_name() else {
        return Ok(());
    };
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let prefix = format!("{}.wal.", db_name.to_string_lossy());

    let mut broken_wals = std::fs::read_dir(parent)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with(&prefix) && name.ends_with(".broken") {
                Some((name, entry.path()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    broken_wals.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in broken_wals.into_iter().skip(1) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

// ─── Stub (DuckDB disabled) ───────────────────────────────────────────

#[cfg(not(feature = "duckdb"))]
/// Stub analytics engine when DuckDB is disabled at compile time.
///
/// All query methods return empty results; the engine is a no-op.
pub struct AnalyticsEngine;

#[cfg(not(feature = "duckdb"))]
impl AnalyticsEngine {
    /// Open a persistent DuckDB database (stub).
    pub fn open<P: AsRef<Path>>(_path: P) -> Result<(Self, bool)> {
        Ok((Self, false))
    }

    /// Open an in-memory DuckDB (stub).
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self)
    }

    /// Initialize base schema (stub).
    pub fn initialize(&self) -> Result<()> {
        Ok(())
    }

    /// Refresh cache (stub).
    pub fn refresh_cache(&self) -> Result<i64> {
        Ok(0)
    }

    /// Force a checkpoint (stub).
    pub fn checkpoint(&self) -> Result<()> {
        Ok(())
    }

    /// Use cached events (stub).
    pub fn use_cached_events(&self) -> Result<()> {
        Ok(())
    }

    /// Create agent views (stub).
    pub fn create_agent_views(&self) -> Result<AgentViewStatus> {
        Ok(AgentViewStatus::default())
    }

    /// Load pricing (stub).
    pub fn load_pricing(&self, _registry: &spur_cost::PricingRegistry) -> Result<()> {
        Ok(())
    }

    /// Daily report (stub).
    pub fn daily_report(&self, _days: u32) -> Result<Vec<DailyRow>> {
        Ok(vec![])
    }

    /// Weekly report (stub).
    pub fn weekly_report(&self, _weeks: u32) -> Result<Vec<WeeklyRow>> {
        Ok(vec![])
    }

    /// Monthly report (stub).
    pub fn monthly_report(&self, _months: u32) -> Result<Vec<MonthlyRow>> {
        Ok(vec![])
    }

    /// Daily report range (stub).
    pub fn daily_report_range(&self, _start: NaiveDate, _end: NaiveDate) -> Result<Vec<DailyRow>> {
        Ok(vec![])
    }

    /// Weekly report range (stub).
    pub fn weekly_report_range(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
    ) -> Result<Vec<WeeklyRow>> {
        Ok(vec![])
    }

    /// Monthly report range (stub).
    pub fn monthly_report_range(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
    ) -> Result<Vec<MonthlyRow>> {
        Ok(vec![])
    }

    /// Live recent sessions (stub).
    pub fn live_recent_sessions(&self, _minutes: u32) -> Result<Vec<LiveBlockRow>> {
        Ok(vec![])
    }

    /// Model breakdown (stub).
    pub fn model_breakdown(&self) -> Result<Vec<ModelRow>> {
        Ok(vec![])
    }

    /// Project breakdown (stub).
    pub fn project_breakdown(&self) -> Result<Vec<ProjectRow>> {
        Ok(vec![])
    }

    /// Session detail (stub).
    pub fn session_detail(&self, _session_id: &str) -> Result<Option<SessionRow>> {
        Ok(None)
    }

    /// Live session snapshot (stub).
    pub fn live_session_snapshot(&self, _session_id: &str) -> Result<Option<LiveSnapshot>> {
        Ok(None)
    }

    /// Raw SQL query (stub).
    pub fn query_json(&self, _sql: &str) -> Result<Vec<serde_json::Value>> {
        Ok(vec![])
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
///
/// `models` is a comma-separated list of distinct model names that ran in
/// this session (was a single `Option<String>`; P0.8 changed it to surface
/// all models when a session switched mid-run).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    pub agent: String,
    pub models: Option<String>,
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
    pub opencode: bool,
    pub kimi: bool,
    pub gemini: bool,
}

/// Intermediate row shape used when copying OpenCode messages from SQLite
/// into DuckDB via the appender. Not part of the public API.
#[cfg(feature = "duckdb")]
#[derive(Debug)]
struct OpenCodeRow {
    timestamp_ms: i64,
    session_id: String,
    model: Option<String>,
    project: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    cost_usd: Option<f64>,
}

/// Intermediate row shape used when copying Kimi _usage events from JSONL
/// into DuckDB via the appender. Not part of the public API.
#[cfg(feature = "duckdb")]
#[derive(Debug)]
struct KimiRow {
    timestamp_ms: i64,
    session_id: String,
    project: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
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
///
/// `models` is a comma-separated list of distinct models (see SessionRow).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveSnapshot {
    pub session_id: String,
    pub agent: String,
    pub models: Option<String>,
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

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn setup_engine() -> AnalyticsEngine {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();
        engine
    }

    /// Idempotency check: re-opening a persistent DB and re-running
    /// initialize + load_pricing across multiple processes must not
    /// accumulate state in PK-indexed tables. The motivating bug was a
    /// FATAL `duplicate key "gpt-4o"` after a prior crash left phantom
    /// ART entries on `pricing`; this test does NOT reproduce that
    /// corrupt-index state (DuckDB's ART is hard to corrupt from SQL
    /// alone) — it only locks in the rebuild contract: schema.sql drops
    /// pricing+scan_manifest before recreating, so any stale rows from
    /// the prior open are gone before load_pricing runs.
    #[test]
    fn initialize_rebuilds_pk_indexed_tables_across_reopens() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("analytics.duckdb");
        let registry = spur_cost::PricingRegistry::with_builtin_prices();

        for _ in 0..3 {
            let (engine, _recovered) = AnalyticsEngine::open(&db_path).unwrap();
            engine.initialize().unwrap();
            engine.load_pricing(&registry).unwrap();
            // After initialize, scan_manifest must be empty — the DROP
            // wipes any prior entries.
            let manifest_rows: i64 = engine
                .conn
                .query_row("SELECT COUNT(*) FROM scan_manifest", [], |r| r.get(0))
                .unwrap();
            assert_eq!(manifest_rows, 0);
        }
    }

    #[test]
    fn open_recovers_corrupt_wal_by_renaming_broken_file() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("analytics.duckdb");
        let wal_path = tmp.path().join("analytics.duckdb.wal");

        {
            let (engine, recovered) = AnalyticsEngine::open(&db_path).unwrap();
            assert!(!recovered);
            engine
                .conn
                .execute_batch(
                    "PRAGMA disable_checkpoint_on_shutdown;
                     CREATE TABLE events(id INTEGER);
                     INSERT INTO events VALUES (1);",
                )
                .unwrap();
        }

        let wal_len = std::fs::metadata(&wal_path).unwrap().len();
        assert!(wal_len >= 128, "expected DuckDB to leave a WAL behind");
        let mut wal = std::fs::OpenOptions::new()
            .write(true)
            .open(&wal_path)
            .unwrap();
        wal.seek(SeekFrom::End(-64)).unwrap();
        wal.write_all(&[0xA5; 64]).unwrap();

        let (_engine, recovered) = AnalyticsEngine::open(&db_path).unwrap();

        assert!(recovered, "open should report WAL recovery");
        assert!(
            !wal_path.exists(),
            "corrupt WAL should be moved out of the way"
        );
        let broken = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.starts_with("analytics.duckdb.wal.") && name.ends_with(".broken"))
            .collect::<Vec<_>>();
        assert_eq!(
            broken.len(),
            1,
            "expected one renamed broken WAL, got {broken:?}"
        );
    }

    #[test]
    fn refresh_cache_checkpoints_wal_after_insert() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("analytics.duckdb");
        let wal_path = tmp.path().join("analytics.duckdb.wal");
        let (engine, recovered) = AnalyticsEngine::open(&db_path).unwrap();
        assert!(!recovered);
        engine.initialize().unwrap();
        for view in [
            "codex_events",
            "kiro_events",
            "opencode_events",
            "kimi_events",
            "gemini_events",
        ] {
            engine.create_empty_stub(view).unwrap();
        }
        engine
            .conn
            .execute_batch(
                "CREATE OR REPLACE VIEW claude_events AS
                 SELECT TIMESTAMP '2026-04-20 10:00:00' AS timestamp,
                        'sess-' || i::VARCHAR AS session_id,
                        'claude' AS agent,
                        'claude-opus-4' AS model,
                        'proj' AS project,
                        1000::BIGINT AS input_tokens,
                        100::BIGINT AS output_tokens,
                        0::BIGINT AS cache_read_tokens,
                        0::BIGINT AS cache_creation_tokens,
                        0.05::DOUBLE AS cost_usd
                 FROM range(100) AS t(i);",
            )
            .unwrap();
        engine.rebuild_unified_views().unwrap();

        let materialized = engine.refresh_cache().unwrap();

        assert_eq!(materialized, 100);
        let wal_size = std::fs::metadata(&wal_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        assert!(
            wal_size <= 4096,
            "refresh_cache should checkpoint WAL to <=4096 bytes, got {wal_size}"
        );
    }

    /// Regression: `refresh_cache()` must not zero out `events_cache` after
    /// `use_cached_events()` has been called. The bug was that
    /// `refresh_cache` sourced its INSERT from `all_events`, but
    /// `use_cached_events` rebinds `all_events` to be `events_cache` itself
    /// — so a stale-detection rebuild becomes "DELETE the cache, then
    /// INSERT from the just-deleted cache" → 0 rows. Fix: source the
    /// rebuild from a stable raw UNION view (`all_events_raw`).
    #[test]
    fn refresh_cache_does_not_self_wipe_after_use_cached_events() {
        let engine = setup_engine();

        engine
            .conn
            .execute_batch(
                "CREATE OR REPLACE VIEW claude_events AS \
                 SELECT * FROM (VALUES \
                    (TIMESTAMP '2026-04-20 10:00:00', 'sess-1', 'claude', \
                     'claude-opus-4', 'proj', 1000::BIGINT, 100::BIGINT, \
                     0::BIGINT, 0::BIGINT, 0.05::DOUBLE) \
                 ) AS t(timestamp, session_id, agent, model, project, \
                        input_tokens, output_tokens, cache_read_tokens, \
                        cache_creation_tokens, cost_usd);",
            )
            .unwrap();
        engine.rebuild_unified_views().unwrap();

        let first = engine.refresh_cache().unwrap();
        assert_eq!(first, 1, "first refresh should materialize 1 row");

        engine.use_cached_events().unwrap();

        engine
            .conn
            .execute_batch("DELETE FROM scan_manifest;")
            .unwrap();

        let second = engine.refresh_cache().unwrap();
        assert_eq!(
            second, first,
            "second refresh must produce the same row count as the first; \
             a different count means refresh_cache lost rows when re-materializing"
        );
        let cache_count: i64 = engine
            .conn
            .query_row("SELECT COUNT(*) FROM events_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            cache_count, 1,
            "refresh_cache must not self-wipe events_cache after use_cached_events"
        );
    }

    fn gemini_fixture_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gemini/two_session_synthetic")
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
    fn create_gemini_view_populates_fixture_and_unified_events() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        for view in [
            "claude_events",
            "codex_events",
            "kiro_events",
            "opencode_events",
            "kimi_events",
        ] {
            engine.create_empty_stub(view).unwrap();
        }
        engine.create_gemini_view(&gemini_fixture_dir()).unwrap();
        engine.rebuild_unified_views().unwrap();

        let gemini_count: i64 = engine
            .conn
            .query_row("SELECT COUNT(*) FROM gemini_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gemini_count, 3);

        let unified_count: i64 = engine
            .conn
            .query_row(
                "SELECT COUNT(*) FROM all_events WHERE agent = 'gemini'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unified_count, 3);

        let totals: (Option<i64>, Option<i64>, Option<i64>) = engine
            .conn
            .query_row(
                "SELECT SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens)
                 FROM all_events WHERE agent = 'gemini'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(totals, (Some(357), Some(75), Some(90)));
    }

    #[test]
    fn test_claude_events_from_fixture() {
        let tmp = TempDir::new().unwrap();
        let claude_root = tmp.path().join("claude");
        let claude_dir = claude_root.join("projects/spur");
        std::fs::create_dir_all(&claude_dir).unwrap();

        // Write a Claude-style JSONL file
        let jsonl_path = claude_dir.join("2026-04-23.jsonl");
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2026-04-23T10:00:00Z","sessionId":"sess-1","message":{{"usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":100,"cache_read_input_tokens":200}},"model":"claude-sonnet-4","id":"msg_1"}},"costUSD":0.05,"requestId":"req_1"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"user","timestamp":"2026-04-23T10:00:30Z","sessionId":"sess-1","message":{{"role":"user","content":"<redacted>"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2026-04-23T10:01:00Z","sessionId":"sess-1","message":{{"usage":{{"input_tokens":2000,"output_tokens":1000,"cache_creation_input_tokens":150,"cache_read_input_tokens":400}},"model":"claude-sonnet-4","id":"msg_2"}},"costUSD":0.10,"requestId":"req_2"}}"#
        )
        .unwrap();

        let engine = setup_engine();
        engine.create_claude_view(&claude_root).unwrap();

        // Query the unified view
        let count: i64 = engine
            .conn
            .query_row("SELECT COUNT(*) FROM claude_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "should have 2 claude events");

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
        let codex_root = tmp.path().join("codex");
        let codex_dir = codex_root.join("sessions/spur");
        std::fs::create_dir_all(&codex_dir).unwrap();

        // Write a Codex-style JSONL file with token-count event envelopes.
        let jsonl_path = codex_dir.join("codex-session.jsonl");
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"turn_context","timestamp":"2026-04-20T10:00:00Z","payload":{{"model":"gpt-5"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"event_msg","timestamp":"2026-04-20T10:01:00Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":0,"total_tokens":1700}},"total_token_usage":{{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":0,"total_tokens":1700}},"model":"gpt-5"}}}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            r#"{"type":"event_msg","timestamp":"2026-04-20T10:02:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":300,"output_tokens":800,"reasoning_output_tokens":0,"total_tokens":2800}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            r#"{"type":"event_msg","timestamp":"2026-04-20T10:03:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":300,"output_tokens":800,"reasoning_output_tokens":0,"total_tokens":2800}}}}"#
        )
        .unwrap();

        let engine = setup_engine();
        engine.create_codex_view(&codex_root).unwrap();

        // Verify delta computation
        let mut stmt = engine
            .conn
            .prepare(
                "SELECT session_id, model, input_tokens, output_tokens, cache_read_tokens \
             FROM codex_events ORDER BY timestamp",
            )
            .unwrap();
        let rows: Vec<(String, Option<String>, i64, i64, i64)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2, "zero-delta row should be excluded");

        // After P0.4 fix: input_tokens is billable-non-cached input, so the
        // cached portion is subtracted. Event 1 raw: input=1000, cached=200 →
        // billable input=800, cache_read=200.
        assert_eq!(rows[0].0, "codex-session");
        assert_eq!(rows[0].1.as_deref(), Some("gpt-5"));
        assert_eq!((rows[0].2, rows[0].3, rows[0].4), (800, 500, 200));

        // Event 2 delta: input=1000, cached_delta=100 → billable=900, cache=100.
        assert_eq!(rows[1].0, "codex-session");
        assert_eq!(rows[1].1.as_deref(), Some("gpt-5"));
        assert_eq!((rows[1].2, rows[1].3, rows[1].4), (900, 300, 100));
    }

    #[test]
    fn test_cost_source_column_values() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();

        // Load pricing for one known model.
        engine
            .conn
            .execute_batch(
                "INSERT INTO pricing VALUES \
                 ('claude-opus-4', 15.0, 75.0, 1.5, 18.75, '2020-01-01', NULL);",
            )
            .unwrap();

        // Override all_events with three manual rows covering each provenance.
        engine
            .conn
            .execute_batch(
                "CREATE OR REPLACE VIEW all_events AS \
                 SELECT * FROM (VALUES \
                    (TIMESTAMP '2026-04-20 10:00:00', 'sess-native', 'claude', 'claude-opus-4', 'proj', \
                     1000::BIGINT, 100::BIGINT, 0::BIGINT, 0::BIGINT, 0.05::DOUBLE), \
                    (TIMESTAMP '2026-04-20 10:05:00', 'sess-priced', 'claude', 'claude-opus-4', 'proj', \
                     1000::BIGINT, 100::BIGINT, 0::BIGINT, 0::BIGINT, NULL::DOUBLE), \
                    (TIMESTAMP '2026-04-20 10:10:00', 'sess-unpriced', 'claude', 'ghost-model-xyz', 'proj', \
                     1000::BIGINT, 100::BIGINT, 0::BIGINT, 0::BIGINT, NULL::DOUBLE) \
                 ) AS t(timestamp, session_id, agent, model, project, \
                        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd);",
            )
            .unwrap();

        let rows: Vec<(String, String)> = {
            let mut stmt = engine
                .conn
                .prepare(
                    "SELECT session_id, cost_source FROM all_events_with_cost \
                     ORDER BY session_id",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ("sess-native".to_string(), "native".to_string()));
        assert_eq!(rows[1], ("sess-priced".to_string(), "priced".to_string()));
        assert_eq!(
            rows[2],
            ("sess-unpriced".to_string(), "unpriced".to_string())
        );

        // Unpriced events must yield NULL computed_cost — no silent $0.
        let unpriced_cost: Option<f64> = engine
            .conn
            .query_row(
                "SELECT computed_cost_usd FROM all_events_with_cost \
                 WHERE session_id = 'sess-unpriced'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            unpriced_cost.is_none(),
            "unpriced event should have NULL computed_cost, not silent $0"
        );
    }

    #[test]
    fn live_recent_sessions_window_uses_utc() {
        let engine = setup_engine();
        engine
            .conn
            .execute_batch("SET TimeZone = 'Asia/Ho_Chi_Minh';")
            .unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let recent_ms = now_ms - 60_000;
        let stale_ms = now_ms - 5 * 60_000;

        engine
            .conn
            .execute_batch(
                r#"
                DROP TABLE IF EXISTS live_recent_sessions_window_events;
                CREATE TABLE live_recent_sessions_window_events (
                    timestamp_ms          BIGINT,
                    session_id            VARCHAR,
                    agent                 VARCHAR,
                    model                 VARCHAR,
                    project               VARCHAR,
                    input_tokens          BIGINT,
                    output_tokens         BIGINT,
                    cache_read_tokens     BIGINT,
                    cache_creation_tokens BIGINT,
                    cost_usd              DOUBLE
                );
                "#,
            )
            .unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO live_recent_sessions_window_events VALUES \
                 (?1, 'recent-session', 'claude', 'claude-sonnet-4', 'spur', \
                  100::BIGINT, 50::BIGINT, 0::BIGINT, 0::BIGINT, 0.01::DOUBLE)",
                duckdb::params![recent_ms],
            )
            .unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO live_recent_sessions_window_events VALUES \
                 (?1, 'stale-session', 'claude', 'claude-sonnet-4', 'spur', \
                  200::BIGINT, 75::BIGINT, 0::BIGINT, 0::BIGINT, 0.02::DOUBLE)",
                duckdb::params![stale_ms],
            )
            .unwrap();
        engine
            .conn
            .execute_batch(
                r#"
                CREATE OR REPLACE VIEW all_events AS
                SELECT
                    epoch_ms(timestamp_ms) AS timestamp,
                    session_id,
                    agent,
                    model,
                    project,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    cost_usd
                FROM live_recent_sessions_window_events;
                "#,
            )
            .unwrap();
        engine
            .conn
            .execute_batch(ALL_EVENTS_WITH_COST_VIEW)
            .unwrap();

        let rows = engine.live_recent_sessions(2).unwrap();
        let session_ids = rows
            .iter()
            .map(|row| row.session_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(session_ids, vec!["recent-session"]);
    }

    #[test]
    fn strip_provider_prefix_handles_known_providers() {
        use super::strip_provider_prefix;

        assert_eq!(
            strip_provider_prefix("anthropic/claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(
            strip_provider_prefix("google/gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(strip_provider_prefix("openai/gpt-5"), "gpt-5");
        assert_eq!(strip_provider_prefix("z-ai/glm-4.6"), "glm-4.6");
        assert_eq!(strip_provider_prefix("moonshotai/kimi-k2"), "kimi-k2");
        assert_eq!(strip_provider_prefix("claude-opus-4-5"), "claude-opus-4-5");
        assert_eq!(strip_provider_prefix("gpt-5-codex"), "gpt-5-codex");
        assert_eq!(strip_provider_prefix(""), "");
        assert_eq!(strip_provider_prefix("/leading-slash"), "leading-slash");
        assert_eq!(strip_provider_prefix("a/b/c"), "b/c");
    }

    #[test]
    fn newest_agent_mtime_detects_opencode_db_changes() {
        use filetime::FileTime;
        use std::time::{Duration, SystemTime};

        let _env_guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let opencode_dir = tmp.path().join(".local/share/opencode");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        let opencode_db = opencode_dir.join("opencode.db");
        std::fs::write(&opencode_db, b"").unwrap();
        env::set_var("OPENCODE_DATA_DIR", &opencode_dir);

        let first = AnalyticsEngine::newest_agent_mtime().unwrap();

        let bumped = SystemTime::now() + Duration::from_secs(60);
        filetime::set_file_mtime(&opencode_db, FileTime::from_system_time(bumped)).unwrap();

        let second = AnalyticsEngine::newest_agent_mtime().unwrap();
        assert!(
            second > first,
            "expected newest_agent_mtime to detect OpenCode DB mtime change"
        );

        env::remove_var("OPENCODE_DATA_DIR");
    }

    #[test]
    fn test_opencode_events_from_sqlite_fixture() {
        // Build a miniature opencode.db mirroring the real Drizzle schema
        // (columns narrowed to what our extractor reads).
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("opencode.db");
        {
            let sconn = rusqlite::Connection::open(&db_path).unwrap();
            sconn
                .execute_batch(
                    r#"
                    CREATE TABLE project (
                        id TEXT PRIMARY KEY,
                        worktree TEXT NOT NULL,
                        name TEXT,
                        time_created INTEGER NOT NULL,
                        time_updated INTEGER NOT NULL,
                        sandboxes TEXT NOT NULL
                    );
                    CREATE TABLE session (
                        id TEXT PRIMARY KEY,
                        project_id TEXT NOT NULL,
                        directory TEXT NOT NULL,
                        title TEXT NOT NULL,
                        version TEXT NOT NULL,
                        time_created INTEGER NOT NULL,
                        time_updated INTEGER NOT NULL
                    );
                    CREATE TABLE message (
                        id TEXT PRIMARY KEY,
                        session_id TEXT NOT NULL,
                        time_created INTEGER NOT NULL,
                        time_updated INTEGER NOT NULL,
                        data TEXT NOT NULL
                    );
                    "#,
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO project VALUES ('p1', '/Volumes/Projects/spur', 'spur', 1, 1, '[]')",
                    [],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO session VALUES ('s1', 'p1', '/Volumes/Projects/spur', 't', 'v1', 1, 1)",
                    [],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO session VALUES ('s2', 'p1', '/Volumes/Projects/spur', 't2', 'v1', 1, 1)",
                    [],
                )
                .unwrap();
            // Two real assistant turns with nonzero cost + one zero-token turn
            // that the extractor must filter out (matches opencode's failed-call shape).
            let m1 = r#"{"role":"assistant","cost":0.01289272,"tokens":{"input":7650,"output":12,"reasoning":0,"cache":{"read":8192,"write":0}},"modelID":"z-ai/glm-5.1","providerID":"openrouter"}"#;
            let m2 = r#"{"role":"assistant","cost":0.0245,"tokens":{"input":3000,"output":500,"reasoning":100,"cache":{"read":0,"write":1024}},"modelID":"moonshotai/kimi-k2.6","providerID":"openrouter"}"#;
            let m_anthropic = r#"{"role":"assistant","cost":0.031,"tokens":{"input":4000,"output":200,"reasoning":0,"cache":{"read":256,"write":128}},"modelID":"anthropic/claude-opus-4-5","providerID":"openrouter"}"#;
            let m_empty = r#"{"role":"assistant","cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"z-ai/glm-5.1","providerID":"openrouter"}"#;
            let m_user = r#"{"role":"user"}"#;
            let base_ms: i64 = 1776580000000; // any realistic unix-ms value
            sconn
                .execute(
                    "INSERT INTO message VALUES ('m1','s1',?1,?1,?2)",
                    rusqlite::params![base_ms, m1],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO message VALUES ('m2','s1',?1,?1,?2)",
                    rusqlite::params![base_ms + 60_000, m2],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO message VALUES ('m3','s1',?1,?1,?2)",
                    rusqlite::params![base_ms + 120_000, m_empty],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO message VALUES ('m4','s1',?1,?1,?2)",
                    rusqlite::params![base_ms + 180_000, m_user],
                )
                .unwrap();
            sconn
                .execute(
                    "INSERT INTO message VALUES ('m5','s2',?1,?1,?2)",
                    rusqlite::params![base_ms + 240_000, m_anthropic],
                )
                .unwrap();
        }

        let engine = setup_engine();
        engine.create_opencode_view(&db_path).unwrap();

        let mut stmt = engine
            .conn
            .prepare(
                "SELECT session_id, agent, model, project, \
                        input_tokens, output_tokens, cache_read_tokens, \
                        cache_creation_tokens, cost_usd \
                 FROM opencode_events ORDER BY timestamp",
            )
            .unwrap();
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
            Option<f64>,
        )> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            rows.len(),
            3,
            "user + zero-token assistant must be filtered"
        );
        assert_eq!(rows[0].0, "s1");
        assert_eq!(rows[0].1, "opencode");
        assert_eq!(rows[0].2.as_deref(), Some("glm-5.1"));
        assert_eq!(rows[0].3.as_deref(), Some("spur"));
        assert_eq!(
            (rows[0].4, rows[0].5, rows[0].6, rows[0].7),
            (7650, 12, 8192, 0)
        );
        assert!((rows[0].8.unwrap() - 0.01289272).abs() < 1e-9);

        assert_eq!(rows[1].2.as_deref(), Some("kimi-k2.6"));
        // reasoning folds into output_tokens
        assert_eq!(
            (rows[1].4, rows[1].5, rows[1].6, rows[1].7),
            (3000, 600, 0, 1024)
        );

        let stored_model: Option<String> = engine
            .conn
            .query_row(
                "SELECT model FROM opencode_events WHERE session_id = 's2' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_model.as_deref(), Some("claude-opus-4-5"));

        // all_events_with_cost must use data.cost as-is (pass-through, no reprice)
        let total_cost: f64 = engine
            .conn
            .query_row(
                "SELECT SUM(computed_cost_usd) FROM all_events_with_cost WHERE agent='opencode'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((total_cost - (0.01289272 + 0.0245 + 0.031)).abs() < 1e-9);
    }

    /// Smoke test against the developer's real `~/.local/share/opencode/opencode.db`.
    ///
    /// Ignored by default — run with `cargo test -p spur-context --lib
    /// smoke_opencode_real_db -- --ignored --nocapture` on a machine that
    /// actually uses OpenCode.
    #[test]
    #[ignore]
    fn smoke_opencode_real_db() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            eprintln!("HOME unset; skipping");
            return;
        };
        let db = home.join(".local/share/opencode/opencode.db");
        if !db.is_file() {
            eprintln!("no opencode db at {}; skipping", db.display());
            return;
        }

        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();
        engine.create_opencode_view(&db).unwrap();

        let (rows, in_sum, out_sum, cost_sum): (i64, Option<i64>, Option<i64>, Option<f64>) =
            engine
                .conn
                .query_row(
                    "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cost_usd) \
                     FROM opencode_events",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
        eprintln!(
            "smoke: rows={} input={:?} output={:?} cost={:?}",
            rows, in_sum, out_sum, cost_sum
        );
        assert!(
            rows > 0,
            "expected at least one opencode row on this dev machine"
        );
    }

    #[test]
    fn test_kimi_events_pair_pre_post() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("kimi").join("sessions");
        let session_dir = sessions.join("projhash").join("sess-uuid");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut f = std::fs::File::create(session_dir.join("context.jsonl")).unwrap();
        for line in [
            r#"{"role":"_system_prompt","content":"sys"}"#,
            r#"{"role":"user","content":"hi"}"#,
            r#"{"role":"_usage","token_count":13000}"#,
            r#"{"role":"assistant","content":"hello"}"#,
            r#"{"role":"_usage","token_count":13500}"#,
            r#"{"role":"tool","content":"t"}"#,
            r#"{"role":"_usage","token_count":14000}"#,
            r#"{"role":"assistant","content":"second"}"#,
            r#"{"role":"_usage","token_count":14100}"#,
        ] {
            writeln!(f, "{}", line).unwrap();
        }

        let engine = setup_engine();
        engine.create_kimi_view(&sessions).unwrap();

        let mut stmt = engine
            .conn
            .prepare(
                "SELECT session_id, agent, model, project, input_tokens, output_tokens, cost_usd \
                 FROM kimi_events ORDER BY timestamp",
            )
            .unwrap();
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            Option<f64>,
        )> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "sess-uuid");
        assert_eq!(rows[0].1, "kimi");
        assert_eq!(rows[0].2.as_deref(), Some("kimi-for-coding"));
        assert_eq!(rows[0].3.as_deref(), Some("projhash"));
        // Turn 1: pre=13000, post=13500 -> input=13000 (first), output=500
        assert_eq!((rows[0].4, rows[0].5), (13000, 500));
        // Turn 2: pre=14000, post=14100 -> input=500 (14000-13500), output=100
        assert_eq!((rows[1].4, rows[1].5), (500, 100));
        assert!(
            rows.iter().all(|r| r.6.is_none()),
            "cost must be NULL for kimi"
        );
    }

    #[test]
    fn test_kimi_events_fallback_on_count_mismatch() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("kimi").join("sessions");
        let session_dir = sessions.join("ph").join("sess");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut f = std::fs::File::create(session_dir.join("context.jsonl")).unwrap();
        // 3 _usage for 1 assistant -> invariant broken, expect fallback path.
        for line in [
            r#"{"role":"user","content":"x"}"#,
            r#"{"role":"_usage","token_count":100}"#,
            r#"{"role":"_usage","token_count":150}"#,
            r#"{"role":"assistant","content":"y"}"#,
            r#"{"role":"_usage","token_count":200}"#,
        ] {
            writeln!(f, "{}", line).unwrap();
        }
        let engine = setup_engine();
        engine.create_kimi_view(&sessions).unwrap();
        let rows: Vec<(i64, i64)> = engine
            .conn
            .prepare("SELECT input_tokens, output_tokens FROM kimi_events ORDER BY timestamp")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 3);
        // Cumulative diffs: 100, 50, 50 -> all as input tokens.
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![100, 50, 50]
        );
        assert!(rows.iter().all(|r| r.1 == 0));
    }

    #[test]
    #[ignore]
    fn smoke_kimi_real_dir() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let sessions = home.join(".kimi/sessions");
        if !sessions.is_dir() {
            return;
        }
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_kimi_view(&sessions).unwrap();
        let (n, i, o): (i64, Option<i64>, Option<i64>) = engine
            .conn
            .query_row(
                "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens) FROM kimi_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        eprintln!("kimi smoke: rows={} input={:?} output={:?}", n, i, o);
        assert!(n > 0, "expected kimi data on this dev machine");
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

    #[test]
    fn report_sql_files_include_cache_columns() {
        assert!(
            DAILY_REPORT_SQL.contains("cache_read_tokens"),
            "daily_report.sql must include cache_read_tokens"
        );
        assert!(
            DAILY_REPORT_SQL.contains("cache_creation_tokens"),
            "daily_report.sql must include cache_creation_tokens"
        );
        assert!(
            WEEKLY_REPORT_SQL.contains("cache_read_tokens"),
            "weekly_report.sql must include cache_read_tokens (was missing)"
        );
        assert!(
            WEEKLY_REPORT_SQL.contains("cache_creation_tokens"),
            "weekly_report.sql must include cache_creation_tokens (was missing)"
        );
        assert!(
            MONTHLY_REPORT_SQL.contains("cache_read_tokens"),
            "monthly_report.sql must include cache_read_tokens (was missing)"
        );
        assert!(
            MONTHLY_REPORT_SQL.contains("cache_creation_tokens"),
            "monthly_report.sql must include cache_creation_tokens (was missing)"
        );
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────
