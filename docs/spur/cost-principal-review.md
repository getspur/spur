# SPUR Cost Subsystem: Principal Engineer Architectural Review

**Author:** Principal Engineer Review (MCTS + First Principles)
**Scope:** `crates/spur-cost/` (4,323 LOC, 38 tests + 3 doc-tests)
**Date:** 2026-04-23
**Classification:** Architecture / Performance / Data Engineering

---

## Executive Summary

The `spur-cost` crate has two mutually incompatible data paths that happen to share type definitions:

1. **Orchestrator Path (SQLite):** `spur-core` writes session metadata and time-based cost estimates to SQLite via `CostTracker`. This path knows about delegation trees, parent/child relationships, and session lifecycle.
2. **Reporter Path (File Ingest):** `Reporter` reads agent-native JSONL logs from `~/.config/claude/`, `~/.codex/sessions/`, etc. This path knows about per-request token usage, model names, and cache hits. It **never reads from the DB**.

This split means:
- **The DB has structural knowledge but no token accuracy.**
- **The ingestors have token accuracy but no structural knowledge.**
- **Reports are produced from the less-structured path** (files), missing delegation trees, project associations, and issue refs that only exist in the DB.
- **Every report generation re-parses ALL historical JSONL files** — O(total_history) regardless of query scope.

This review proposes unifying the paths behind an **incremental materialization layer** with DuckDB as the analytical engine, reducing report generation from O(history) to O(1) for cached windows and enabling analytical queries that are impossible in SQLite.

---

## Round 1: First Principles — What Are We Actually Computing?

### 1.1 The Fundamental Operation

At its core, `spur-cost` answers one question repeatedly:

> **"What did we spend, on what, when, and how is it trending?"**

This is a **time-series aggregation** problem over append-only logs. The mathematical structure is:

- **Raw events:** `e_i = (t_i, agent_i, model_i, project_i, session_i, input_i, output_i, cache_create_i, cache_read_i, cost_i)`
- **Query:** `AGGREGATE(e_i WHERE t_i ∈ [T_start, T_end]) GROUP BY dimension`
- **Dimensions:** agent, model, project, session, day, week, month
- **Aggregates:** SUM(tokens), SUM(cost), COUNT(sessions), RATE(dcost/dt)

### 1.2 Invariants of the Source Data

| Invariant | Implication |
|-----------|-------------|
| Agent JSONL logs are **append-only** | We never modify historical events |
| Agent logs are **monotonically growing** | File mtime + file size are sufficient change detectors |
| Cumulative counters (Codex) are **non-decreasing** | Deltas are always ≥ 0 |
| Deduplication key is **idempotent** | Re-processing the same file produces the same events |
| Time is **mostly-ordered** within a session | Out-of-order events are rare and bounded |

These invariants scream **incremental computation**. We should never re-parse a file whose `(path, mtime, size)` hash hasn't changed.

### 1.3 The Current Complexity Class

| Operation | Current | Optimal |
|-----------|---------|---------|
| Daily report (today) | O(total_history) | O(today's events) or O(1) with pre-aggregation |
| Live report (last 30 min) | O(total_history) | O(active sessions) |
| Weekly report | O(total_history) | O(7 days) or O(1) with pre-aggregation |
| Monthly report | O(total_history) | O(30 days) or O(1) with pre-aggregation |
| Model cost breakdown | O(total_history) | O(1) with pre-aggregation |
| "Cost by project over last 90 days" | O(total_history) | O(1) with pre-aggregation |

The current implementation is **O(total_history)** for *every* query because `IngestionPipeline::load_range()` globs all files, parses all lines, deduplicates globally, sorts, and only then filters.

---

## Round 2: Incremental Calculation Architecture

### 2.1 The Core Insight

> **If source data is append-only, aggregation can be incremental.**

Instead of re-deriving reports from raw logs on every query, we should:

1. **Ingest once:** Parse JSONL → `TokenEvent` → persist to a structured store
2. **Materialize aggregates:** Maintain running totals per (day, agent, model, project)
3. **Query aggregates:** Read from pre-computed summaries, not raw events
4. **Invalidate selectively:** Only re-process changed files

### 2.2 Proposed Architecture: The Materialization Ledger

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────────┐
│  Agent JSONL    │────▶│  Incremental     │────▶│  Event Store        │
│  (Claude/Codex) │     │  Ingestor        │     │  (Parquet/DuckDB)   │
└─────────────────┘     └──────────────────┘     └─────────────────────┘
                              │                             │
                              ▼                             ▼
                       ┌─────────────┐              ┌──────────────┐
                       │ File State  │              │ Pre-aggregated│
                       │ Registry    │              │ Cubes         │
                       │ (mtime hash)│              │ (daily cubes) │
                       └─────────────┘              └──────────────┘
                                                            │
                                                            ▼
                                                     ┌─────────────┐
                                                     │  Reporter   │
                                                     │  O(1) reads │
                                                     └─────────────┘
```

### 2.3 The File State Registry

A SQLite table (small, local, fast) tracks ingestion state:

```sql
CREATE TABLE ingest_state (
    file_path TEXT PRIMARY KEY,
    file_mtime_ns INTEGER NOT NULL,  -- nanoseconds for precision
    file_size INTEGER NOT NULL,
    content_hash TEXT,               -- blake3 of file content
    last_line_count INTEGER,         -- lines successfully parsed last time
    last_ingest_at TEXT,             -- ISO8601
    events_count INTEGER,            -- events extracted
    first_event_at TEXT,             -- min timestamp in file
    last_event_at TEXT               -- max timestamp in file
);

CREATE INDEX idx_ingest_state_time ON ingest_state(last_event_at);
```

**Incremental logic:**
```rust
fn ingest_incremental(&mut self) -> Result<IngestDelta> {
    let mut delta = IngestDelta::default();

    for path in self.discover_paths() {
        let metadata = fs::metadata(&path)?;
        let mtime = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos() as i64;
        let size = metadata.len() as i64;

        let state = self.db.query_row(
            "SELECT file_mtime_ns, file_size FROM ingest_state WHERE file_path = ?",
            [&path],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        ).optional()?;

        match state {
            Some((old_mtime, old_size)) if old_mtime == mtime && old_size == size => {
                // File unchanged — skip entirely
                continue;
            }
            Some((_, old_size)) if size > old_size => {
                // File grew — seek to old_size, parse only new lines
                delta.append(self.parse_from_offset(&path, old_size as u64)?);
            }
            _ => {
                // New or shrunk file — full re-parse
                delta.append(self.parse_full(&path)?);
            }
        }

        // Update state
        self.db.execute(
            "INSERT OR REPLACE INTO ingest_state ...",
            params![path, mtime, size, ...]
        )?;
    }

    Ok(delta)
}
```

**Why this matters:**
- A typical Claude Code session generates ~1-5MB of JSONL over hours
- With incremental ingestion, adding 100 new lines to a 50,000-line file costs O(100), not O(50,100)
- For a developer running SPUR daily for a year, this is the difference between 2-second and 200ms report generation

### 2.4 Pre-Aggregated Cubes

Instead of storing raw `TokenEvent`s and aggregating at query time, maintain **daily cubes**:

```rust
#[derive(Debug, Clone)]
struct DailyCube {
    date: NaiveDate,           // 2026-04-23
    agent: String,             // "claude"
    model: String,             // "claude-sonnet-4"
    project: Option<String>,   // "spur-core"

    // Aggregates
    event_count: u64,
    session_count: u64,        // distinct sessions
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,

    // For rate computation
    first_event_at: DateTime<Utc>,
    last_event_at: DateTime<Utc>,
}
```

**Cube update is additive:**
```rust
impl DailyCube {
    fn merge_event(&mut self, event: &TokenEvent) {
        self.event_count += 1;
        self.input_tokens += event.input_tokens;
        self.output_tokens += event.output_tokens;
        self.cost_usd += event.cost_usd.unwrap_or(0.0);
        self.last_event_at = self.last_event_at.max(event.timestamp);
        // session_count updated via HyperLogLog or exact set (small cardinality)
    }
}
```

**Query translation:**
| Report Type | Current (O(history)) | With Cubes (O(days)) |
|-------------|---------------------|----------------------|
| Daily report | Aggregate all events, group by day | Read cubes directly |
| Weekly report | Aggregate all events, group by week | Sum 7 cubes |
| Monthly report | Aggregate all events, group by month | Sum 30 cubes |
| Live report | Filter all events by time window | Sum cubes for today + filter raw events for last N min |
| Model breakdown | Aggregate all events, group by model | Sum cubes grouped by model |

For "last 30 days", we read ~30 × (agents × models × projects) cubes. Even with 10 agents × 20 models × 50 projects = 10,000 cube rows/day, 30 days = 300,000 rows. DuckDB scans this in milliseconds.

### 2.5 The Delegation Tree Problem

Current `build_session_tree()` in `reports.rs` produces a flat forest because the file-based path doesn't have `parent_session` information. The DB has this but the reporter doesn't read from it.

**Solution:** A unified event store should join `TokenEvent` with `SessionRecord` on `session_id` (or a derived join key), producing enriched events that carry both token accuracy and structural knowledge.

```sql
-- Enriched event: combines ingest accuracy with orchestrator structure
CREATE TABLE enriched_events (
    -- From ingestor
    timestamp TIMESTAMP,
    session_id TEXT,
    agent TEXT,
    model TEXT,
    input_tokens UBIGINT,
    output_tokens UBIGINT,
    cache_creation_tokens UBIGINT,
    cache_read_tokens UBIGINT,
    cost_usd DOUBLE,
    source_file TEXT,

    -- From orchestrator DB (joined)
    parent_session TEXT,
    project TEXT,
    issue_ref TEXT,
    role TEXT,           -- 'brain' or 'worker'
    task_summary TEXT
);
```

This join is a **one-time cost at ingestion**, not a query-time cost.

---

## Round 3: DuckDB for Advanced Analytics

### 3.1 Why DuckDB?

SQLite is an OLTP database. `spur-cost` does OLAP. The mismatch is fundamental:

| Capability | SQLite | DuckDB | Why It Matters |
|------------|--------|--------|----------------|
| Columnar storage | No | Yes | Aggregates over 10M events are 10-100× faster |
| Vectorized execution | No | Yes | SIMD-optimized SUM/GROUP BY |
| JSONL direct query | No | Yes | `SELECT * FROM read_json_auto('~/.codex/sessions/**/*.jsonl')` |
| Window functions | Basic | Rich | Running costs, rolling averages, percentiles |
| Time-series functions | No | Yes | `date_trunc`, `generate_series` for gap filling |
| Parquet export | No | Yes | Archive old data cheaply |
| Aggregation pushdown | No | Yes | Only reads columns needed |
| Embedded (no server) | Yes | Yes | Same deployment model as SQLite |
| Zero external deps | Yes | Yes | Single C++ lib, Rust bindings via `duckdb-rs` |

### 3.2 DuckDB as a Query Engine Over JSONL

DuckDB can query JSONL files directly without ingestion:

```sql
-- Query Claude JSONL directly
SELECT
    timestamp,
    sessionId AS session_id,
    message.model,
    message.usage.input_tokens,
    message.usage.output_tokens,
    costUSD AS cost_usd
FROM read_json_auto('~/.config/claude/projects/**/*.jsonl',
    format='newline_delimited',
    columns={timestamp='TIMESTAMP', sessionId='VARCHAR', message='STRUCT(model VARCHAR, usage STRUCT(input_tokens BIGINT, output_tokens BIGINT))', costUSD='DOUBLE'}
);
```

**But this is not the endgame** — it's a migration aid. Direct JSONL queries are still O(history) per query. The value is:

1. **Schema inference:** DuckDB's `read_json_auto` handles nested structures better than our hand-rolled serde
2. **Migration path:** We can replace our custom parsers with DuckDB views initially
3. **Complex analytics:** Cohort analysis, cost anomaly detection, model efficiency ratios

### 3.3 The Hybrid Architecture: SQLite + DuckDB

```
┌─────────────────────────────────────────────────────────────┐
│                      SPUR Cost Subsystem                     │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │  SQLite      │    │  DuckDB      │    │  Parquet     │  │
│  │  (Metadata)  │◄──►│  (Analytics) │◄──►│  (Archive)   │  │
│  │              │    │              │    │              │  │
│  │ - sessions   │    │ - raw events │    │ - old cubes  │  │
│  │ - delegation │    │ - daily cubes│    │ - yearly     │  │
│  │ - ingest_state│   │ - model stats│    │   rollups    │  │
│  │              │    │ - project    │    │              │  │
│  │              │    │   analytics  │    │              │  │
│  └──────────────┘    └──────────────┘    └──────────────┘  │
│         ▲                   ▲                                │
│         │                   │                                │
│    ┌────┴───────────────────┴────┐                          │
│    │    Incremental Ingestor     │                          │
│    │  (Claude/Codex/Kiro →       │                          │
│    │   TokenEvent → Enrich →     │                          │
│    │   Persist to both stores)   │                          │
│    └─────────────────────────────┘                          │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

**SQLite remains the source of truth for:**
- Session lifecycle (start, end, status)
- Delegation graph (brain_session → worker_session)
- Project and issue_ref metadata
- Ingestion state (file registry)

**DuckDB becomes the analytics engine for:**
- Time-series aggregation (daily, weekly, monthly)
- Model efficiency analysis (cost per token, tokens per second)
- Cohort tracking ("How much did we spend on issue #42?")
- Anomaly detection ("This session cost 5× the 90th percentile")
- Ad-hoc SQL queries for power users

### 3.4 Advanced Analytics Enabled by DuckDB

Queries that are painful or impossible in SQLite:

```sql
-- 1. Cost efficiency by model (cost per 1K tokens)
SELECT
    model,
    SUM(cost_usd) / SUM(input_tokens + output_tokens) * 1000 AS cost_per_1k_tokens,
    COUNT(DISTINCT session_id) AS session_count
FROM enriched_events
WHERE timestamp > now() - INTERVAL '30 days'
GROUP BY model
ORDER BY cost_per_1k_tokens DESC;

-- 2. Cumulative cost over time (running total)
SELECT
    date_trunc('day', timestamp) AS day,
    SUM(cost_usd) AS daily_cost,
    SUM(SUM(cost_usd)) OVER (ORDER BY date_trunc('day', timestamp)) AS cumulative_cost
FROM enriched_events
GROUP BY day
ORDER BY day;

-- 3. Cost anomaly detection (sessions > 2 stddev above mean)
WITH session_costs AS (
    SELECT session_id, SUM(cost_usd) AS total_cost
    FROM enriched_events
    GROUP BY session_id
),
stats AS (
    SELECT AVG(total_cost) AS mean_cost, STDDEV(total_cost) AS stddev_cost
    FROM session_costs
)
SELECT sc.session_id, sc.total_cost, s.mean_cost, s.stddev_cost
FROM session_costs sc
CROSS JOIN stats s
WHERE sc.total_cost > s.mean_cost + 2 * s.stddev_cost;

-- 4. Cache hit ratio by model
SELECT
    model,
    SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(input_tokens), 0) AS cache_hit_pct,
    SUM(cache_creation_tokens) * 100.0 / NULLIF(SUM(input_tokens), 0) AS cache_write_pct
FROM enriched_events
GROUP BY model;

-- 5. Project burn rate with gap filling
SELECT
    date_trunc('day', timestamp) AS day,
    project,
    SUM(cost_usd) AS daily_cost,
    SUM(SUM(cost_usd)) OVER (
        PARTITION BY project
        ORDER BY date_trunc('day', timestamp)
        ROWS BETWEEN 6 PRECEDING AND CURRENT ROW
    ) AS rolling_7day_cost
FROM enriched_events
GROUP BY day, project
ORDER BY day, project;
```

### 3.5 Parquet Archiving Strategy

As data grows, old events can be archived to Parquet:

```rust
// Monthly: archive events older than 90 days to Parquet
fn archive_old_events(&mut self) -> Result<()> {
    let cutoff = Utc::now() - Duration::days(90);

    // Export to Parquet
    self.duckdb.execute(&format!(
        "COPY (
            SELECT * FROM enriched_events
            WHERE timestamp < '{}'
        ) TO 'archive/2026-Q1.parquet' (FORMAT PARQUET, COMPRESSION 'ZSTD')",
        cutoff.format("%Y-%m-%d")
    ))?;

    // Create view that unions hot (DuckDB) + cold (Parquet) data
    self.duckdb.execute(
        "CREATE OR REPLACE VIEW all_events AS
         SELECT * FROM enriched_events
         UNION ALL
         SELECT * FROM read_parquet('archive/*.parquet')"
    )?;

    // Delete archived rows from hot store
    self.duckdb.execute(
        "DELETE FROM enriched_events WHERE timestamp < ?",
        params![cutoff]
    )?;

    Ok(())
}
```

**Cost:** Parquet files are ~10× smaller than JSONL and query faster than raw files.

---

## Round 4: The Unified Data Path Problem

### 4.1 Current State: Two Divergent Truths

```rust
// Path A: Orchestrator writes this
CostTracker::start_session(SessionRecord {
    id: "sess-123",
    agent: "claude",
    project: "spur-core",
    issue_ref: "#42",
    parent_session: Some("sess-parent"),
    estimated_cost_usd: 0.50,  // HEURISTIC — not actual
    ..,
});

// Path B: Reporter reads this (from JSONL)
TokenEvent {
    session_id: "sess-123",
    agent: "claude",
    model: Some("claude-sonnet-4"),
    input_tokens: 15000,       // ACTUAL — from API response
    output_tokens: 5000,
    cost_usd: Some(0.12),      // ACTUAL — computed from tokens
    ..,
    // BUT: no project, no issue_ref, no parent_session
}
```

### 4.2 The Unification Strategy

**Step 1: Make `TokenEvent` the canonical fact**

Per-event token usage from agent logs is the ground truth. Time-based heuristics are estimates. The DB should store actuals where available.

**Step 2: Enrich events at ingestion time**

```rust
struct EnrichmentContext {
    session_map: HashMap<String, SessionRecord>,  // From SQLite
}

fn enrich_event(event: TokenEvent, ctx: &EnrichmentContext) -> EnrichedEvent {
    let session = ctx.session_map.get(&event.session_id);
    EnrichedEvent {
        // From ingestor (accurate)
        timestamp: event.timestamp,
        input_tokens: event.input_tokens,
        output_tokens: event.output_tokens,
        cost_usd: event.cost_usd,
        model: event.model,

        // From orchestrator DB (structural)
        project: session.and_then(|s| s.project.clone()),
        issue_ref: session.and_then(|s| s.issue_ref.clone()),
        parent_session: session.and_then(|s| s.parent_session.clone()),
        role: session.map(|s| s.role.clone()),
    }
}
```

**Step 3: Update the orchestrator to write actuals**

When a session ends, the orchestrator should:
1. Call the ingestor for that agent's recent JSONL
2. Get actual token usage for the session
3. Update the DB row with actuals (not just estimates)

```rust
fn end_session_with_actuals(&mut self, session_id: &str) -> Result<()> {
    let estimate = self.get_session_estimate(session_id)?;

    // Try to get actuals from agent logs
    let actuals = self.ingestor
        .query_session(session_id)
        .map(|events| compute_actuals(&events))
        .unwrap_or_else(|| estimate); // Fallback to estimate

    self.db.execute(
        "UPDATE sessions SET
            ended_at = ?, status = 'completed',
            input_tokens = ?, output_tokens = ?,
            cache_creation_tokens = ?, cache_read_tokens = ?,
            estimated_cost_usd = ?  -- now actual cost
         WHERE id = ?",
        params![
            Utc::now(),
            actuals.input_tokens, actuals.output_tokens,
            actuals.cache_creation_tokens, actuals.cache_read_tokens,
            actuals.cost_usd,
            session_id
        ]
    )?;

    Ok(())
}
```

**Step 4: Reporter queries the unified store**

```rust
impl Reporter {
    fn daily_report(&self, range: ReportRange) -> Result<Vec<DailyReport>> {
        // Query DuckDB cubes (fast) instead of re-parsing JSONL (slow)
        self.duckdb.query(
            "SELECT date, agent, model, project,
                    SUM(input_tokens), SUM(output_tokens), SUM(cost_usd),
                    COUNT(DISTINCT session_id)
             FROM daily_cubes
             WHERE date BETWEEN ? AND ?
             GROUP BY date, agent, model, project
             ORDER BY date",
            params![range.start, range.end]
        )
    }
}
```

---

## Round 5: Concrete Recommendations & Migration Path

### 5.1 Priority Matrix

| Priority | Item | Effort | Impact | Owner |
|----------|------|--------|--------|-------|
| P0 | Fix the two-path split (unified enrichment) | Medium | Critical | Core |
| P0 | Incremental file ingestion (state registry) | Low | High | Cost |
| P1 | DuckDB integration for analytics | Medium | High | Cost |
| P1 | Pre-aggregated daily cubes | Low | High | Cost |
| P2 | Parquet archiving | Low | Medium | Cost |
| P2 | Kiro ingestor implementation | Medium | Medium | Integrations |
| P3 | Cost anomaly detection queries | Low | Medium | Analytics |
| P3 | Replace time-based estimates with actuals | Medium | Low | Core |

### 5.2 Phase 1: Incremental Ingestion (1-2 days)

**Goal:** Stop re-parsing unchanged files.

1. Add `ingest_state` table to SQLite schema
2. Implement `IncrementalIngestor` wrapper that checks file metadata before parsing
3. Add `parse_from_offset` for append-only file growth
4. Maintain backward compatibility: `IngestionPipeline` remains the public API

**Validation:**
- Benchmark: report generation time should not grow with history size
- Test: modify a file, verify only that file is re-parsed

### 5.3 Phase 2: Daily Cubes (2-3 days)

**Goal:** O(1) report generation for common queries.

1. Add `daily_cubes` table (SQLite initially, DuckDB later)
2. Implement `CubeStore::merge_events(events)` — additive update
3. Modify `Reporter` to query cubes first, fall back to raw events for "today"
4. Add `cube_rebuild()` for backfill / corruption recovery

**Validation:**
- Daily report for 1 year of data should complete in <100ms
- Cube totals should match raw aggregation (property-based test)

### 5.4 Phase 3: DuckDB Integration (3-5 days)

**Goal:** Advanced analytics + columnar performance.

1. Add `duckdb-rs` dependency (or `libduckdb-sys` for lower-level control)
2. Create `AnalyticsEngine` trait:
   ```rust
   trait AnalyticsEngine {
       fn ingest_events(&mut self, events: &[EnrichedEvent]) -> Result<()>;
       fn query_daily(&self, range: ReportRange) -> Result<Vec<DailyReport>>;
       fn query_custom(&self, sql: &str) -> Result<QueryResult>;
       fn export_parquet(&self, path: &Path) -> Result<()>;
   }
   ```
3. Implement `DuckDbEngine` and `SqliteEngine` (fallback)
4. Feature-gate DuckDB: `features = ["duckdb-analytics"]`

**Validation:**
- All existing tests pass with SQLite fallback
- DuckDB tests verify analytical queries
- Benchmark: 10× faster aggregation on 1M events

### 5.5 Phase 4: Unified Data Path (5-7 days)

**Goal:** Single source of truth with both accuracy and structure.

1. Modify `spur-core::orchestrator` to call ingestor on session end
2. Update `SessionRecord` to store actual token usage
3. Implement `EnrichmentContext` join at ingestion time
4. Deprecate `CostTracker::end_session()` in favor of `end_session_with_actuals()`
5. Update `Reporter` to use enriched events exclusively

**Validation:**
- Integration test: start session → generate agent log → end session → verify DB has actuals
- Report should show both accurate costs AND correct project/issue breakdowns

### 5.6 Code-Level Issues to Fix Immediately

| File | Issue | Severity | Fix |
|------|-------|----------|-----|
| `pricing.rs` | `gpt-5.3-codex` → `gpt-5.2-codex` alias dead-ends | Medium | Fix to `gpt-5-codex` |
| `db.rs` | Zero unit tests for CRUD | High | Add tests |
| `tracker.rs` | Zero unit tests for `CostTracker` | High | Add tests |
| `reporter.rs` | Zero unit tests for report generation | High | Add tests |
| `reports.rs` | `session_count` counts events not sessions | Medium | Use `HashSet` or HyperLogLog |
| `tracker.rs` | `duration.as_secs() as i64` truncates sub-seconds | Low | Use `as_millis()` or `Duration` directly |
| `ingest/kiro.rs` | Complete stub | Medium | Implement or remove |
| `Cargo.toml` | `tokio` declared but unused | Low | Remove |
| `Cargo.toml` | `thiserror` declared but `anyhow` used everywhere | Low | Either use `thiserror` or remove it |

---

## Appendix A: Performance Model

### A.1 Current Performance Characteristics

Assume a developer using SPUR for 1 year:
- ~250 working days
- ~20 sessions/day across 3 agents
- ~100 events/session (token counts, turns, etc.)
- ~50 lines per JSONL file per session
- Average JSONL line: ~500 bytes

**Total data volume:**
- Events: 250 × 20 × 100 = 500,000 events
- JSONL files: 250 × 20 × 3 = 15,000 files
- Raw JSONL size: 15,000 × 50 × 500B = ~375 MB
- Parsed in-memory: ~2-4 GB (Rust structs are fat)

**Current query costs:**
| Query | Operations | Estimated Time |
|-------|-----------|----------------|
| Daily report | Glob 15K files, parse 375MB, dedup 500K events, aggregate | 2-5 seconds |
| Live report (30 min) | Same as daily, then filter by timestamp | 2-5 seconds |
| Weekly report | Same as daily, group by week | 2-5 seconds |

**With incremental cubes:**
| Query | Operations | Estimated Time |
|-------|-----------|----------------|
| Daily report | Read 30 cube rows | 1-5 ms |
| Live report | Read today's cube + last 30 min raw | 10-50 ms |
| Weekly report | Sum 7 cube rows | 1-5 ms |

### A.2 Memory Pressure Analysis

Current `IngestionPipeline` holds all events in memory:
```rust
pub fn load_all(&self) -> Result<Vec<TokenEvent>> {
    // ALL events from ALL history
    let mut all = Vec::new();
    for path in self.discover_paths() {
        all.extend(ingestor.load_file(&path)?);
    }
    all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(all)
}
```

At 500K events × ~200 bytes/event = 100MB just for `Vec<TokenEvent>`. With dedup HashSet, string allocations, and sorting, this easily reaches 500MB-1GB.

**With incremental + cubes:** Peak memory is bounded by one file's events, not all history.

---

## Appendix B: DuckDB Schema Design

```sql
-- Raw events (hot store, last 90 days)
CREATE TABLE enriched_events (
    timestamp TIMESTAMP NOT NULL,
    session_id VARCHAR NOT NULL,
    agent VARCHAR NOT NULL,
    model VARCHAR,
    project VARCHAR,
    issue_ref VARCHAR,
    parent_session VARCHAR,
    role VARCHAR,
    input_tokens UBIGINT,
    output_tokens UBIGINT,
    cache_creation_tokens UBIGINT,
    cache_read_tokens UBIGINT,
    cost_usd DOUBLE,
    source_file VARCHAR
);

-- Pre-aggregated cubes
CREATE TABLE daily_cubes (
    date DATE NOT NULL,
    agent VARCHAR NOT NULL,
    model VARCHAR NOT NULL,
    project VARCHAR,
    event_count UBIGINT,
    session_count UBIGINT,
    input_tokens UBIGINT,
    output_tokens UBIGINT,
    cache_creation_tokens UBIGINT,
    cache_read_tokens UBIGINT,
    cost_usd DOUBLE,
    first_event_at TIMESTAMP,
    last_event_at TIMESTAMP,
    PRIMARY KEY (date, agent, model, project)
);

-- Indexes for fast lookups
CREATE INDEX idx_events_time ON enriched_events(timestamp);
CREATE INDEX idx_events_session ON enriched_events(session_id);
CREATE INDEX idx_events_project ON enriched_events(project);

-- Materialized view: model efficiency
CREATE MATERIALIZED VIEW model_efficiency AS
SELECT
    model,
    SUM(cost_usd) / NULLIF(SUM(input_tokens + output_tokens), 0) * 1000000 AS cost_per_million_tokens,
    SUM(input_tokens) / NULLIF(SUM(output_tokens), 0) AS input_output_ratio,
    COUNT(DISTINCT session_id) AS session_count
FROM enriched_events
GROUP BY model;
```

---

## Summary

The `spur-cost` crate has solid foundations — correct pricing math, clean ingestor abstractions, and good test coverage for the pricing domain. But it has an architectural blind spot: **it treats every report query as a full historical recomputation**.

The path forward is:

1. **Immediate (this week):** Add incremental file-state registry. Stop re-parsing unchanged files.
2. **Short-term (next 2 weeks):** Add pre-aggregated daily cubes. Make common reports O(days) instead of O(history).
3. **Medium-term (next month):** Integrate DuckDB for analytical queries. Enable cohort analysis, anomaly detection, and ad-hoc SQL.
4. **Long-term (next quarter):** Unify the DB and file paths. Every session should have both structural metadata (from orchestrator) and accurate token usage (from agent logs).

The guiding principle:

> **Compute once at write time, read O(1) at query time. Append-only logs demand incremental materialization, not repeated re-aggregation.**
