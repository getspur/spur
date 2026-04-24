# SPUR Analytics Architecture: DuckDB as Unified Query Interface

**Date:** 2026-04-23
**Status:** Architectural Design
**Principle:** SQLite for operations, DuckDB for analytics. No migrations.

---

## The Architectural Principle

> **"Operational state lives in SQLite. Analytical truth lives in DuckDB."**

| Concern | Technology | Data | Why |
|---------|-----------|------|-----|
| **Operations** | SQLite | Sessions, delegations, issues, issue deps | ACID OLTP, proven, embedded |
| **Context** | DuckDB | Decisions, observations, entities, relationships, embeddings | Analytical queries, graph, vector |
| **Analytics** | DuckDB | Unified views joining all of the above | Single SQL interface, optimizer, cross-domain joins |

**Constraint:** No migration of cost.db or beads.db. DuckDB queries them where they live.

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           SPUR Orchestrator                                  │
│                                                                              │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐     │
│  │   Brain     │   │   Workers   │   │  spur-mcp   │   │  Beads API  │     │
│  │   (ACP)     │   │  (ACP)      │   │  (MCP Svr)  │   │  (br CLI)   │     │
│  └──────┬──────┘   └──────┬──────┘   └──────┬──────┘   └──────┬──────┘     │
│         │                 │                 │                 │            │
│         └─────────────────┴─────────────────┴─────────────────┘            │
│                              │                                             │
│                    ┌─────────┴─────────┐                                   │
│                    │   Orchestrator    │                                   │
│                    │   (async/tokio)   │                                   │
│                    └─────────┬─────────┘                                   │
│                              │                                             │
│         ┌────────────────────┼────────────────────┐                        │
│         │                    │                    │                        │
│         ▼                    ▼                    ▼                        │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐             │
│  │ CostTracker │    │   Beads     │    │   ContextEngine     │             │
│  │  (SQLite)   │    │  (SQLite)   │    │     (DuckDB)        │             │
│  │             │    │             │    │                     │             │
│  │ sessions    │    │  issues     │    │  decisions          │             │
│  │ delegation_ │    │  dependencies│   │  observations       │             │
│  │ log         │    │  comments   │    │  entities (P2)      │             │
│  │             │    │  labels     │    │  relationships (P2) │             │
│  │             │    │             │    │  embeddings (P3)    │             │
│  └──────┬──────┘    └──────┬──────┘    └──────────┬──────────┘             │
│         │                  │                      │                        │
│         │   ┌──────────────┘                      │                        │
│         │   │  (spur-context writes to DuckDB)    │                        │
│         │   │                                     │                        │
│         │   │   ┌─────────────────────────────────┘                        │
│         │   │   │                                                        │
│         ▼   ▼   ▼                                                        │
│  ┌─────────────────────────────────────────────────────────────┐         │
│  │              DuckDB Analytics Engine                          │         │
│  │                                                               │         │
│  │  ATTACH 'cost.db' AS cost (TYPE SQLITE);                      │         │
│  │  ATTACH 'beads.db' AS beads (TYPE SQLITE);                    │         │
│  │  -- decisions, observations already native                    │         │
│  │                                                               │         │
│  │  CREATE VIEW unified_sessions AS ...                          │         │
│  │  CREATE VIEW issue_cost_summary AS ...                        │         │
│  │  CREATE VIEW decision_effectiveness AS ...                    │         │
│  │                                                               │         │
│  └─────────────────────────────────────────────────────────────┘         │
│                              │                                             │
│                              ▼                                             │
│  ┌─────────────────────────────────────────────────────────────┐         │
│  │                    Query Interfaces                           │         │
│  │                                                               │         │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │         │
│  │  │   Reporter   │  │   Context    │  │  MCP Tools   │      │         │
│  │  │  (cost rpt)  │  │  (assembly)  │  │  (recall_)   │      │         │
│  │  └──────────────┘  └──────────────┘  └──────────────┘      │         │
│  └─────────────────────────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Round 1: First Principles — Why This Separation?

### 1.1 What Is Operations vs Analytics?

**Operations (SQLite):**
- High-frequency, low-latency writes
- Must survive crashes without corruption
- Simple lookups by primary key
- Transactional integrity is paramount
- Examples: `start_session()`, `end_session()`, `create_issue()`, `update_issue_status()`

**Analytics (DuckDB):**
- Read-heavy, complex queries
- Joins across multiple domains
- Aggregations, window functions, graph traversals
- Can tolerate slight staleness
- Examples: "Cost per issue over last 30 days", "Find similar failed decisions"

### 1.2 Why SQLite Is Right for Operations

| Property | SQLite | DuckDB |
|----------|--------|--------|
| Write latency | Sub-millisecond | ~1-5ms |
| ACID durability | Battle-tested (20+ years) | Good, but newer |
| Single-file portability | `.db` file | Directory or single file |
| WAL mode | Yes (concurrent reads) | Yes |
| Binary size (embedded) | ~1MB | ~30-50MB |
| Recovery tooling | `sqlite3` CLI, extensive | Growing |

For SPUR's operational data (sessions, issues), the write pattern is:
- Sessions: ~20-50 writes/day
- Issues: ~5-20 writes/day
- Delegations: ~20-100 writes/day

SQLite handles this effortlessly. DuckDB would too, but SQLite is smaller, simpler, and already works.

### 1.3 Why DuckDB Is Right for Analytics

| Property | DuckDB | SQLite |
|----------|--------|--------|
| Columnar scans | Yes (vectorized) | No (row-oriented) |
| Parallel aggregation | Yes (multi-thread) | Limited |
| Window functions | Rich | Basic |
| Graph queries (DuckPGQ) | Yes | No |
| Vector search (Lance) | Yes | No |
| Cross-database queries | Yes (ATTACH) | No |
| Query JSONL directly | Yes (`read_json_auto`) | No |

The key insight: **analytics queries are not just "faster" in DuckDB — they are "possible."** SQLite cannot do graph traversal or vector similarity search at all.

### 1.4 The CQRS Pattern

This architecture is **CQRS** (Command Query Responsibility Segregation):

```
Commands (writes) ──► SQLite ──► Operational state
                              │
                              │  ATTACH / IMPORT
                              ▼
Queries (reads) ─────► DuckDB ──► Analytical truth
```

**CQRS is not eventual consistency.** The SQLite databases are the source of truth. DuckDB reads them directly via the SQLite scanner — there is no replication lag, no background sync, no data duplication.

---

## Round 2: How DuckDB Reads SQLite

### 2.1 The SQLite Scanner Extension

DuckDB can attach SQLite databases natively:

```sql
-- Load extension (if not auto-loaded)
INSTALL sqlite;
LOAD sqlite;

-- Attach operational databases
ATTACH 'cost.db' AS cost (TYPE SQLITE);
ATTACH '.beads/beads.db' AS beads (TYPE SQLITE);
```

Now DuckDB can query SQLite tables as if they were native:

```sql
-- Query cost data
SELECT * FROM cost.sessions WHERE status = 'completed';

-- Query beads data
SELECT * FROM beads.issues WHERE status = 'open';

-- Cross-domain join (this is the power)
SELECT
    i.title,
    i.status,
    COUNT(d.id) AS decision_count,
    SUM(s.estimated_cost_usd) AS total_cost,
    AVG(s.duration_seconds) AS avg_duration
FROM beads.issues i
LEFT JOIN cost.sessions s ON s.issue_ref = i.id
LEFT JOIN decisions d ON d.session_id = s.id
WHERE i.status = 'open'
GROUP BY i.id, i.title, i.status
ORDER BY total_cost DESC;
```

### 2.2 Performance Characteristics

| Query Type | SQLite Scanner Performance | Native DuckDB |
|------------|---------------------------|---------------|
| Single-table filter | ~1-2× slower than SQLite | Baseline |
| Small join (<10K rows) | ~2-3× slower | Baseline |
| Large aggregation | ~3-5× slower | Baseline |
| Cross-domain join | **Only possible** | Baseline |

**The SQLite scanner translates DuckDB SQL to SQLite SQL.** It pushes down filters and projections but cannot push down complex aggregations or joins. For SPUR's scale (~10K sessions, ~1K issues), the performance is acceptable.

### 2.3 Materialized Views for Hot Queries

For queries that run frequently, create materialized views in DuckDB:

```sql
-- Create a native DuckDB table from SQLite data
CREATE TABLE issue_summary AS
SELECT
    i.id AS issue_id,
    i.title,
    i.status,
    i.priority,
    i.created_at,
    COUNT(s.id) AS session_count,
    SUM(s.estimated_cost_usd) AS total_cost,
    SUM(s.duration_seconds) AS total_duration,
    SUM(s.input_tokens) AS total_input_tokens,
    SUM(s.output_tokens) AS total_output_tokens
FROM beads.issues i
LEFT JOIN cost.sessions s ON s.issue_ref = i.id
GROUP BY i.id, i.title, i.status, i.priority, i.created_at;

-- Refresh on demand (or schedule)
INSERT OR REPLACE INTO issue_summary ...
```

This gives you:
- **Native DuckDB performance** for hot queries
- **No data duplication risk** (can always rebuild from source)
- **Explicit refresh control** (not hidden background sync)

---

## Round 3: MCTS Branch A — Pure Scanner (No Materialization)

Every query goes through the SQLite scanner live.

**Simulation A1: Daily cost report**
```sql
SELECT
    strftime('%Y-%m-%d', s.started_at) AS day,
    s.agent,
    SUM(s.estimated_cost_usd) AS daily_cost
FROM cost.sessions s
WHERE s.started_at > now() - INTERVAL '30 days'
GROUP BY day, s.agent;
```
→ SQLite scanner pushes down filter and projection to SQLite
→ SQLite handles aggregation natively
→ Result streams back to DuckDB
→ **Performance: Good** (~100ms for 10K sessions)

**Simulation A2: Context assembly with cost signal**
```sql
SELECT
    d.action,
    d.outcome,
    s.estimated_cost_usd,
    s.duration_seconds,
    s.input_tokens + s.output_tokens AS total_tokens
FROM decisions d
JOIN cost.sessions s ON s.id = d.session_id
WHERE d.outcome = 'success'
ORDER BY s.estimated_cost_usd DESC
LIMIT 5;
```
→ SQLite scanner reads matching sessions
→ DuckDB joins with native decisions table
→ **Performance: Acceptable** (~50-200ms)

**Simulation A3: Complex multi-domain analytics**
```sql
WITH issue_decisions AS (
    SELECT
        i.id AS issue_id,
        i.title,
        d.id AS decision_id,
        d.action,
        d.outcome,
        s.estimated_cost_usd,
        s.duration_seconds
    FROM beads.issues i
    JOIN cost.sessions s ON s.issue_ref = i.id
    JOIN decisions d ON d.session_id = s.id
    WHERE i.status = 'open'
)
SELECT
    issue_id,
    title,
    COUNT(decision_id) AS decisions,
    SUM(estimated_cost_usd) AS cost,
    AVG(duration_seconds) AS avg_duration,
    SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) * 1.0 / COUNT(*) AS success_rate
FROM issue_decisions
GROUP BY issue_id, title
ORDER BY cost DESC;
```
→ Multiple SQLite scanner round-trips
→ DuckDB handles final aggregation
→ **Performance: Slow** (~500ms-2s for complex joins)

**Branch A Score: +0.5** — Simple queries work well, complex queries suffer.

---

## Round 4: MCTS Branch B — Materialized Views in DuckDB

Create native DuckDB tables for hot data, refresh periodically.

**Schema:**
```sql
-- Native DuckDB tables (rebuilt from SQLite on refresh)
CREATE TABLE cost_sessions (
    id TEXT PRIMARY KEY,
    agent TEXT,
    role TEXT,
    project TEXT,
    issue_ref TEXT,
    started_at TIMESTAMP,
    ended_at TIMESTAMP,
    status TEXT,
    duration_seconds INTEGER,
    estimated_cost_usd DOUBLE,
    input_tokens BIGINT,
    output_tokens BIGINT,
    cache_creation_tokens BIGINT,
    cache_read_tokens BIGINT,
    model TEXT
);

CREATE TABLE beads_issues (
    id TEXT PRIMARY KEY,
    title TEXT,
    description TEXT,
    status TEXT,
    priority INTEGER,
    issue_type TEXT,
    assignee TEXT,
    created_at TIMESTAMP,
    updated_at TIMESTAMP,
    closed_at TIMESTAMP,
    external_ref TEXT
);
```

**Refresh strategy:**
```rust
impl AnalyticsEngine {
    /// Refresh materialized views from SQLite sources
    pub fn refresh(&self) -> Result<RefreshStats> {
        let mut stats = RefreshStats::default();

        // 1. Refresh cost data (incremental: only changed sessions)
        let last_refresh = self.get_last_refresh("cost_sessions")?;
        self.conn.execute(
            "INSERT OR REPLACE INTO cost_sessions
             SELECT * FROM sqlite_scan('cost.db',
                 'SELECT * FROM sessions WHERE updated_at > ?')
            ",
            params![last_refresh]
        )?;
        stats.cost_sessions += self.conn.rows_affected();

        // 2. Refresh beads data (full refresh — issues table is small)
        self.conn.execute(
            "INSERT OR REPLACE INTO beads_issues
             SELECT * FROM sqlite_scan('.beads/beads.db', 'SELECT * FROM issues')",
            []
        )?;
        stats.beads_issues += self.conn.rows_affected();

        self.set_last_refresh("cost_sessions", Utc::now())?;
        Ok(stats)
    }
}
```

**Simulation B1: Daily cost report**
→ Query native DuckDB table
→ Columnar scan, vectorized aggregation
→ **Performance: Excellent** (~5-10ms)

**Simulation B2: Complex multi-domain analytics**
→ All tables are native DuckDB
→ Single optimizer sees all data
→ Efficient join ordering
→ **Performance: Excellent** (~20-50ms)

**Simulation B3: Data freshness concern**
```
User: "I just closed an issue, why does the report still show it open?"
→ Materialized view hasn't refreshed
→ Refresh is manual or scheduled
→ User confusion
```
→ **Mitigation:** Refresh on write hooks, or expose refresh button in TUI

**Branch B Score: +0.75** — Fast queries, but requires refresh management.

---

## Round 5: MCTS Branch C — Hybrid (Hot Tables in DuckDB, Cold via Scanner)

Materialize frequently-queried tables, query cold data via scanner.

**Hot tables (materialized):**
- `cost_sessions` — queried in every report
- `beads_issues` — small, frequently joined
- `beads_dependencies` — needed for DAG analysis

**Cold tables (scanner):**
- `cost.delegation_log` — queried rarely
- `beads.comments` — only for detail views
- `beads.labels` — only for filtering

**Simulation C1: Common query paths are fast**
→ 90% of queries hit materialized tables
→ Native DuckDB performance

**Simulation C2: Rare deep queries still work**
→ "Show all comments on expensive issues"
→ Comments table via scanner, issues table native
→ Acceptable performance for rare query

**Simulation C3: Memory and disk usage**
→ Materialized tables: ~10-50MB
→ DuckDB database file: ~100MB total
→ Acceptable

**Branch C Score: +0.85** — Best balance of performance and simplicity.

---

## Round 6: The Write Path — What Actually Goes Into DuckDB?

### 6.1 Operational Writes → SQLite (unchanged)

```rust
// spur-core orchestrator
impl Orchestrator {
    fn start_session(&self, params: SessionStart) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.cost_tracker.as_ref().unwrap()
            .start_session(&id, &params.agent, &params.project)?;
        // Beads issue update (if applicable)
        self.beads.update_issue_session(&params.issue_ref, &id)?;
        Ok(id)
    }
}
```

### 6.2 Context Writes → DuckDB (as designed)

```rust
// spur-context engine
impl ContextEngine {
    fn record_decision(&self, session_id: &str, action: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO decisions (id, session_id, action, created_at, outcome)
             VALUES (?, ?, ?, ?, 'pending')",
            params![id, session_id, action, Utc::now()]
        )?;
        Ok(id)
    }
}
```

### 6.3 Analytics Refresh → DuckDB (materialized views)

```rust
// spur-analytics (or part of spur-context)
impl AnalyticsEngine {
    fn refresh_from_sources(&self) -> Result<()> {
        // Incremental refresh from SQLite sources
        self.refresh_cost_sessions()?;
        self.refresh_beads_issues()?;
        Ok(())
    }
}
```

---

## Round 7: The Unified Query Interface

### 7.1 What Users (and the Brain) See

All queries go through DuckDB. The brain doesn't know about SQLite:

```sql
-- Brain asks: "What are my open issues and their cost?"
SELECT
    i.title,
    i.priority,
    COUNT(s.id) AS sessions,
    SUM(s.estimated_cost_usd) AS cost,
    STRING_AGG(DISTINCT d.outcome, ', ') AS outcomes
FROM beads_issues i
LEFT JOIN cost_sessions s ON s.issue_ref = i.id
LEFT JOIN decisions d ON d.session_id = s.id
WHERE i.status = 'open'
GROUP BY i.id, i.title, i.priority
ORDER BY cost DESC;

-- Brain asks: "Find similar past decisions that were cheap"
WITH similar AS (
    SELECT id, action, score
    FROM lance_vector_search('decision_embeddings', ?embedding, 20)
)
SELECT
    s.id,
    s.action,
    s.score,
    cs.estimated_cost_usd,
    cs.duration_seconds,
    d.outcome,
    s.score / NULLIF(cs.estimated_cost_usd, 0) AS value_per_dollar
FROM similar s
JOIN decisions d ON d.id = s.id
JOIN cost_sessions cs ON cs.id = d.session_id
WHERE d.outcome = 'success'
ORDER BY value_per_dollar DESC
LIMIT 5;

-- Brain asks: "What entities are related to expensive decisions?"
SELECT
    e.name,
    e.entity_type,
    COUNT(d.id) AS decision_count,
    SUM(cs.estimated_cost_usd) AS total_cost
FROM entities e
JOIN relationships r ON r.source_entity = e.id
JOIN observations o ON o.content ILIKE '%' || e.name || '%'
JOIN decisions d ON d.id = o.decision_id
JOIN cost_sessions cs ON cs.id = d.session_id
GROUP BY e.id, e.name, e.entity_type
ORDER BY total_cost DESC
LIMIT 10;
```

### 7.2 What Happens Under the Hood

```sql
-- beads_issues is a native DuckDB table (materialized)
-- cost_sessions is a native DuckDB table (materialized)
-- decisions is a native DuckDB table (context engine writes here)
-- entities, relationships are native DuckDB tables (Phase 2)
-- lance_vector_search is a DuckDB table function (Phase 3)
```

All tables are **native DuckDB**. The SQLite scanner is only used during refresh, not during query time.

---

## The Concrete Architecture

### File Layout

```
~/.local/share/spur/
├── cost.db              # SQLite: operational cost data (unchanged)
├── context.db           # DuckDB: decisions, observations, entities, relationships
├── context.db.wal       # DuckDB WAL
└── embeddings/
    ├── decision_embeddings.lance    # Phase 3
    ├── observation_embeddings.lance # Phase 3
    └── entity_embeddings.lance    # Phase 3

~/projects/my-project/
└── .beads/
    └── beads.db         # SQLite: issues, dependencies (unchanged)
```

### Crate Responsibilities

| Crate | Database | Writes | Reads | Refresh |
|-------|----------|--------|-------|---------|
| `spur-cost` | SQLite `cost.db` | Sessions, delegations | — | — |
| `spur-context` | DuckDB `context.db` | Decisions, observations, entities | Context assembly | Refreshes `cost_sessions`, `beads_issues` |
| `spur-core` | Both | Delegates to both | Orchestrates | Triggers refresh |
| `spur-cli` / TUI | DuckDB | — | Reports, analytics | — |

### The Refresh Contract

```rust
/// When to refresh materialized views
pub enum RefreshTrigger {
    /// Refresh after every session end (low latency, higher overhead)
    OnSessionEnd,
    /// Refresh on explicit user request
    OnDemand,
    /// Refresh periodically (e.g., every 5 minutes)
    Periodic(Duration),
    /// Refresh before each report query
    BeforeQuery,
}
```

**Recommendation:** `OnSessionEnd` for cost data (small incremental change), `OnDemand` for beads data (changes less frequently).

---

## Synthesis: The Final Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      WRITE PATH                                  │
│                                                                  │
│  spur-core ──► cost_tracker ──► SQLite cost.db                  │
│            ──► context_engine ──► DuckDB context.db             │
│            ──► beads CLI ──► SQLite .beads/beads.db             │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                     REFRESH PATH                                 │
│                                                                  │
│  spur-context ──► sqlite_scan(cost.db) ──► cost_sessions (native)│
│               ──► sqlite_scan(beads.db) ──► beads_issues (native)│
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                     QUERY PATH                                   │
│                                                                  │
│  TUI / CLI / Brain ──► DuckDB context.db                        │
│                    ──► Native tables: decisions, observations   │
│                    ──► Native tables: entities, relationships   │
│                    ──► Native tables: cost_sessions, beads_issues│
│                    ──► Table functions: lance_vector_search     │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Principles enforced:**
1. ✅ **SQLite for operations** — cost.db and beads.db unchanged
2. ✅ **DuckDB for analytics** — all queries go through DuckDB
3. ✅ **No migrations** — SQLite databases stay where they are
4. ✅ **Native performance** — hot tables materialized in DuckDB
5. ✅ **Cross-domain joins** — single optimizer sees all data
6. ✅ **Extensible** — Phase 2 graph, Phase 3 vector fit naturally

---

## Implementation Plan

### Phase 0: Infrastructure (1-2 days)

1. Add `duckdb` to workspace `Cargo.toml`
2. Create `spur-context` crate with DuckDB connection
3. Verify DuckDB builds in CI (use `DUCKDB_DOWNLOAD_LIB=1`)
4. Add SQLite scanner extension loading test

### Phase 1: Context Engine + Materialized Views (3-5 days)

1. Implement `ContextEngine` with decisions/observations tables
2. Add `AnalyticsEngine` with materialized view refresh
3. Create `cost_sessions` and `beads_issues` materialized tables
4. Implement refresh logic (incremental for cost, full for beads)
5. Update `Orchestrator` to hold both `cost_tracker` and `context`

### Phase 2: Unified Reporter (2-3 days)

1. Create `Reporter` that queries DuckDB (not JSONL)
2. Port daily/weekly/monthly reports to DuckDB SQL
3. Add cross-domain reports (cost per issue, cost per project)
4. Deprecate JSONL re-parsing path

### Phase 3: Context Assembly with Cost Signals (2 days)

1. Update `assemble_context()` to include cost data
2. Add cost-adjusted relevance scoring
3. Add MCP tools for cost-aware recall

---

## The First-Principle Validation

> **"Operations are writes. Analytics are reads. The write path should be simple and durable. The read path should be powerful and flexible. SQLite is simple and durable. DuckDB is powerful and flexible. This is the right separation."**

The user was right: **DuckDB as first-class citizen for analytics, SQLite as operational backbone.** No migration needed. The architecture unifies at the query layer, not the storage layer.
