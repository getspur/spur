# SPUR Cost + Context: Second-Order MCTS Review

**Author:** Principal Engineer Review (Second-Order Thinking + MCTS)
**Scope:** `spur-cost` × `spur-context` architectural interaction
**Date:** 2026-04-23
**Basis:** Approved `spur-context` spec (DuckDB-centric) + existing `spur-cost` (SQLite-centric)

---

## The New Problem Statement

The approved `spur-context` spec (2026-04-13) mandates DuckDB as the unified context engine. `spur-cost` currently uses SQLite. This creates a **two-database reality** in a single orchestrator:

```rust
pub struct Orchestrator {
    pub cost_tracker: Option<CostTracker>,      // SQLite
    pub context: Option<ContextEngine>,         // DuckDB
    // ...
}
```

**First-order question:** Should we migrate `spur-cost` to DuckDB?

**Second-order questions:**
- What happens when a query needs both cost data and context data?
- What are the build implications of two bundled C dependencies?
- What happens when Phase 2 graph queries need token usage per decision?
- What happens when the context engine needs historical cost trends for ranking?
- What is the operational complexity of two embedded databases in one process?

---

## Round 1: First Principles — What Are We Actually Building?

### The Data Gravity Map

| Data Type | Current Home | Natural Queries | Cross-Domain Queries |
|-----------|-------------|-----------------|---------------------|
| Session lifecycle | SQLite (`sessions`) | "How much did we spend?" | "What decisions led to expensive sessions?" |
| Delegation graph | SQLite (`delegation_log`) | "Who delegated what?" | "Which brain decisions produced successful worker outcomes?" |
| Token usage | JSONL (Claude/Codex/Kiro) | "Cost per model" | "Which models produce the most valuable context?" |
| Decisions | DuckDB (`decisions`) | "What did the brain decide?" | "What was the cost of each decision?" |
| Observations | DuckDB (`observations`) | "What did the worker produce?" | "What's the cost-per-deliverable trend?" |
| Entities/Graph | DuckDB (Phase 2) | "What concepts are related?" | "Which concepts appear in high-cost sessions?" |
| Embeddings | Lance (Phase 3) | "Similar past tasks" | "Are expensive tasks semantically similar?" |

**Key insight:** The cross-domain queries are the *valuable* ones. "Find auth-related tasks sorted by cost" is more useful than either query alone. The spec recognizes this:

> "DuckDB's SQLite scanner extension (`ATTACH 'cost.db' AS cost (TYPE SQLITE)`) can provide cross-database queries without migrating spur-cost."

But this is a **workaround**, not an architecture. Workarounds accumulate into technical debt.

### The Second-Order Insight

> **If `spur-context` is already committed to DuckDB, the question is not "SQLite vs DuckDB for cost" but "One database or two?"**

Two databases means:
- Two connection pools (even if single-threaded, two file handles)
- Two backup/restore procedures
- Two schema migration paths
- Two WAL files, two cache footprints
- Joins across databases require extensions or application-layer joins
- Application code must know which data lives where

One database means:
- Single connection, single file (or single directory for DuckLake)
- Native SQL joins between cost and context
- Single backup/restore
- Single schema migration path
- But: requires migrating `spur-cost` from SQLite to DuckDB

---

## Round 2: MCTS Branch A — Keep Separate (SQLite + DuckDB)

This is the current implicit architecture: `spur-cost` stays on SQLite, `spur-context` uses DuckDB.

### 2.1 How Cross-Database Queries Work

**Option A1: DuckDB SQLite Scanner Extension**
```sql
-- Inside DuckDB context engine
ATTACH 'cost.db' AS cost (TYPE SQLITE);

-- Cross-database query
SELECT d.action, d.outcome, s.estimated_cost_usd, s.duration_seconds
FROM decisions d
JOIN cost.sessions s ON d.session_id = s.id
WHERE d.outcome = 'success'
ORDER BY s.estimated_cost_usd DESC;
```

**Second-order effects:**
- The SQLite scanner is an **extension** that must be loaded at runtime
- It works by translating DuckDB queries to SQLite queries — performance is worse than native
- The `cost.db` file must be accessible from the DuckDB process (same filesystem)
- File locking: SQLite's single-writer lock may conflict with DuckDB's read queries
- The extension may not be available in all DuckDB builds (requires `sqlite_scanner` extension)

**Option A2: Application-Layer Joins**
```rust
// In orchestrator or reporter
let decisions = context.query_decisions_for_project("spur-core")?;
let session_ids: Vec<&str> = decisions.iter().map(|d| d.session_id.as_str()).collect();
let costs = cost_tracker.query_costs_for_sessions(&session_ids)?;
// Merge in Rust code
```

**Second-order effects:**
- N+1 query pattern if not batched
- No query optimizer can optimize the join
- Memory pressure: load all data into Rust structs before filtering
- Cannot push down predicates (e.g., `WHERE cost > 1.0`) to either database

**Option A3: Duplicate Cost Data into DuckDB**
```sql
-- In DuckDB: create a materialized view of cost data
CREATE TABLE cost_sessions AS
SELECT * FROM sqlite_scan('cost.db', 'SELECT * FROM sessions');

-- Periodically refresh
INSERT OR REPLACE INTO cost_sessions ...
```

**Second-order effects:**
- Data duplication (source of truth split)
- Refresh logic adds complexity
- Stale data risk
- But: queries are fast and native

### 2.2 Build Complexity (Two C Dependencies)

```toml
[workspace.dependencies]
# Current
rusqlite = { version = "0.32", features = ["bundled"] }  # C: SQLite

# Adding for context
duckdb = { version = "1", features = ["bundled"] }       # C++: DuckDB
```

**Second-order effects:**
- `rusqlite` bundled compiles SQLite C (~1-2 minutes)
- `duckdb` bundled compiles DuckDB C++ (~5-15 minutes, or downloads prebuilt)
- Two build scripts running in parallel
- Two sets of linking flags
- Two sets of platform-specific quirks
- Debug builds compile both from source → long compile times
- Release builds: DuckDB can use prebuilt, SQLite always compiles

**Mitigation:** `DUCKDB_DOWNLOAD_LIB=1` avoids C++ compilation, but SQLite still compiles.

### 2.3 Operational Complexity

| Concern | One DB | Two DBs |
|---------|--------|---------|
| Backup | `cp spur.db` | `cp cost.db && cp context.db` |
| Migration | One schema version | Two schema versions |
| Corruption recovery | One file to check | Two files to check |
| Disk usage | One WAL, one cache | Two WALs, two caches |
| Query planning | Optimizer sees all data | Optimizer sees half the data |
| Testing | One test DB setup | Two test DB setups |

### 2.4 MCTS Simulation: Keep Separate

**Simulation A1: Developer adds "cost per decision" feature**
```
Developer: "I need to show the brain which past decisions were expensive"
→ Query decisions from DuckDB
→ Query costs from SQLite
→ Merge in Rust
→ N+1 query if naive, complex batching if optimized
→ Feature takes 2 days instead of 2 hours
→ Code is harder to test
```
**Score: +0.3** (works, but friction accumulates)

**Simulation A2: Phase 3 hybrid RAG needs cost signals**
```
Context engine: "Rank similar past decisions by cost efficiency"
→ Vector search in Lance for semantic similarity
→ Join with DuckDB observations
→ Need cost data to compute "cost per token"
→ SQLite scanner extension query is slow
→ Application-layer join is complex
→ Feature is deferred or simplified
```
**Score: +0.1** (architectural friction limits feature quality)

**Simulation A3: Build on fresh CI runner**
```
CI: cargo build --workspace
→ Compiles SQLite C (rusqlite bundled)
→ Compiles/downloads DuckDB (duckdb bundled/download)
→ Both pass, total build time +2-5 minutes
→ No failures, but slower CI
```
**Score: +0.5** (acceptable but suboptimal)

**Branch A UCT Score: +0.3** → **Works but accumulates friction**

---

## Round 3: MCTS Branch B — Migrate Cost to DuckDB (Unified)

Migrate `spur-cost` from SQLite to DuckDB, making `spur-context` and `spur-cost` share one database.

### 3.1 What Changes in spur-cost

**Schema migration:**
```sql
-- Current SQLite schema
CREATE TABLE sessions (...);
CREATE TABLE delegation_log (...);

-- New: In DuckDB (same schema, different engine)
CREATE TABLE sessions (...);
CREATE TABLE delegation_log (...);

-- New: Pre-aggregated cubes (native to DuckDB, faster than SQLite)
CREATE TABLE daily_cubes (...);

-- New: Ingested token events from agent JSONL
CREATE TABLE token_events (...);
```

**API changes:**
- `CostTracker` uses `duckdb::Connection` instead of `rusqlite::Connection`
- Query methods use DuckDB's SQL dialect (mostly compatible, some differences)
- `bundled` feature replaces `rusqlite` dependency

### 3.2 Benefits of Unification

**Native cross-domain queries:**
```sql
-- Single query, single optimizer, single execution engine
SELECT
    d.action,
    d.outcome,
    s.estimated_cost_usd,
    s.duration_seconds,
    s.input_tokens,
    s.output_tokens,
    s.input_tokens + s.output_tokens AS total_tokens,
    s.estimated_cost_usd / NULLIF(s.input_tokens + s.output_tokens, 0) * 1000000 AS cost_per_million_tokens
FROM decisions d
JOIN sessions s ON d.session_id = s.id
WHERE d.outcome = 'success'
ORDER BY cost_per_million_tokens DESC
LIMIT 10;
```

**Single database file:**
```
~/.local/share/spur/context.db   -- One file for everything
```

**Context assembly enriched with cost signals:**
```sql
-- "Find similar past decisions, but prefer cheap ones"
WITH similar AS (
    SELECT id, action, score
    FROM lance_vector_search('decision_embeddings.lance', ?embedding, 20)
)
SELECT s.id, s.action, s.score,
       sess.estimated_cost_usd,
       sess.duration_seconds,
       s.score * (1.0 / (1.0 + sess.estimated_cost_usd)) AS cost_adjusted_score
FROM similar s
JOIN decisions d ON d.id = s.id
JOIN sessions sess ON sess.id = d.session_id
ORDER BY cost_adjusted_score DESC
LIMIT 5;
```

### 3.3 Costs of Migration

**Development cost:**
- Replace `rusqlite` with `duckdb` in `spur-cost`
- Update all SQL for DuckDB dialect (minor: SQLite and DuckDB are both PostgreSQL-like)
- Rewrite `db.rs` CRUD methods (~500 lines)
- Update tests (~35 tests)
- Risk: `rusqlite` has features `spur-cost` relies on (WAL mode, `bundled`, specific pragmas)

**Migration for users:**
- Existing `cost.db` files need migration or users start fresh
- DuckDB can import SQLite: `CALL sqlite_attach('cost.db')` then `CREATE TABLE ... AS SELECT * FROM ...`
- But this is a one-time migration script

**Dependency impact:**
```toml
# Before
[dependencies]
rusqlite = { workspace = true }  # Only C dep

# After
[dependencies]
duckdb = { workspace = true }    # C++ dep, but prebuilt download works
```

- Remove `rusqlite` from workspace (or keep for other crates)
- `spur-cost` binary size: same (SQLite was statically linked, DuckDB can be dynamic)
- Build time: same or better (DuckDB prebuilt download ~30s vs SQLite C compile ~2min)

### 3.4 MCTS Simulation: Unified DuckDB

**Simulation B1: Developer adds "cost per decision" feature**
```
Developer: "I need to show the brain which past decisions were expensive"
→ Single SQL query joining decisions + sessions
→ DuckDB optimizer handles the join
→ Feature takes 2 hours
→ Code is simple, testable
```
**Score: +0.9**

**Simulation B2: Phase 3 hybrid RAG with cost signals**
```
Context engine: "Rank similar past decisions by cost efficiency"
→ Single SQL query: vector search + join sessions + compute cost/token
→ No application-layer merging
→ Feature ships as designed
```
**Score: +0.9**

**Simulation B3: spur-cost migration effort**
```
Engineer: "Replace rusqlite with duckdb in spur-cost"
→ Update db.rs (~500 lines)
→ Update tracker.rs (~200 lines)
→ Update tests (~35 tests)
→ Most SQL is compatible
→ Takes 2-3 days
→ One-time cost
```
**Score: +0.6** (upfront cost, one-time)

**Simulation B4: Long-term maintenance**
```
One database file
One schema migration path
One backup procedure
One query language
One set of performance tuning knobs
→ Lower cognitive load
→ Easier onboarding
```
**Score: +0.8**

**Branch B UCT Score: +0.80** → **SELECT THIS BRANCH**

---

## Round 4: MCTS Branch C — Hybrid (DuckDB Analytics + SQLite OLTP)

Keep `spur-cost` OLTP operations (session start/end, delegation log) in SQLite, but replicate/analytics data into DuckDB for reporting and context assembly.

### 4.1 How It Works

**Write path (dual write):**
```rust
fn end_session(&mut self, session_id: &str) -> Result<()> {
    // 1. Update SQLite (source of truth for OLTP)
    self.sqlite.execute("UPDATE sessions SET ...", params![])?;

    // 2. Insert into DuckDB (analytics + context)
    self.duckdb.execute("INSERT INTO sessions ...", params![])?;

    Ok(())
}
```

**Read path:**
- OLTP queries (session status, active delegations) → SQLite
- Analytics queries (reports, context assembly) → DuckDB

### 4.2 Second-Order Effects

**Data consistency:**
- Two writes per operation → risk of divergence
- Need transactions spanning both databases (impossible with embedded DBs)
- If DuckDB write fails, SQLite has data DuckDB doesn't
- If SQLite write fails, DuckDB may have stale data

**Code complexity:**
```rust
pub struct HybridCostTracker {
    sqlite: rusqlite::Connection,  // OLTP
    duckdb: duckdb::Connection,    // Analytics
}
```
- Every write method becomes twice as complex
- Every read method requires choosing the right database
- Testing requires setting up both databases

**Performance:**
- Dual writes = 2× write latency
- For SPUR's write volume (sessions start/end, few per minute), this is negligible
- But the complexity cost is real

### 4.3 MCTS Simulation: Hybrid

**Simulation C1: Data drift after error**
```
Orchestrator: end_session() → SQLite succeeds, DuckDB fails (disk full)
→ SQLite has the session as "completed"
→ DuckDB has the session as "running"
→ Next context query: "show me recent completed sessions" → misses one
→ Report query: "daily cost" → under-reports
→ Debugging requires checking two databases
```
**Score: -0.4** (data consistency is hard)

**Simulation C2: Developer confusion**
```
New engineer: "Where do I read session status?"
→ "SQLite for OLTP, DuckDB for analytics"
→ "How do I know which is which?"
→ "Read the doc... or just guess and hope"
→ Bugs from reading wrong database
```
**Score: -0.3** (cognitive load)

**Branch C UCT Score: -0.15** → **REJECT**

---

## Round 5: Second-Order Analysis — What Happens in 6 Months?

### Scenario: Phase 2 Graph + Cost Analytics

**With separate databases (Branch A):**
```sql
-- In DuckDB: Need cost data for graph analytics
-- Option: SQLite scanner (slow) or duplicated data (stale)

-- Graph query: "Find most expensive code paths"
SELECT e.name, SUM(s.estimated_cost_usd) AS total_cost
FROM GRAPH_TABLE(knowledge_graph
    MATCH (file:entities)-[:CONTAINS]->(func:entities)
    COLUMNS (file.name, func.name)
) g
JOIN ??? sessions ON ???  -- Where do we join?
```

The `???` is the problem. Graph traversal produces entities. We need to join with sessions that mention those entities. With separate databases, this join is either:
- Slow (SQLite scanner)
- Complex (application-layer)
- Stale (duplicated data)

**With unified DuckDB (Branch B):**
```sql
-- Single query, single optimizer
SELECT e.name, SUM(s.estimated_cost_usd) AS total_cost
FROM GRAPH_TABLE(knowledge_graph
    MATCH (file:entities)-[:CONTAINS]->(func:entities)
    COLUMNS (file.id AS file_id, file.name)
) g
JOIN observations o ON o.content ILIKE '%' || g.name || '%'
JOIN sessions s ON s.id = o.session_id
GROUP BY e.name
ORDER BY total_cost DESC;
```

### Scenario: Phase 3 Vector + Cost Ranking

**With separate databases:**
```sql
-- Vector search in DuckDB (via Lance extension)
-- Cost data in SQLite
-- Join in application code
```

The RRF (Reciprocal Rank Fusion) query from the spec needs cost data:
```sql
-- Spec's Phase 3 query, but now we want cost-adjusted ranking
SELECT
    v.id,
    v.action,
    1.0 / (60.0 + v.vector_rank) +
    1.0 / (60.0 + g.graph_rank) +
    1.0 / (60.0 + c.cost_rank) AS rrf_score  -- Added cost signal
FROM ...
```

With separate databases, adding `c.cost_rank` requires either:
- Extending the data duplication pipeline
- Slow cross-database queries
- Abandoning the feature

**With unified DuckDB:** The query works as written.

---

## Round 6: The Migration Path (If Branch B Is Selected)

### Phase 0: Preparation (1 day)

1. Audit `spur-cost` SQLite usage:
   - `db.rs`: schema, CRUD, views
   - `tracker.rs`: high-level API
   - `reporter.rs`: report queries
   - Tests: 35 unit tests

2. Identify DuckDB incompatibilities:
   - `AUTOINCREMENT` → DuckDB uses `CREATE SEQUENCE` + `DEFAULT NEXTVAL('seq')`
   - SQLite-specific pragmas (WAL mode, journal_mode) → Not needed in DuckDB
   - `rusqlite` API (`query_row`, `execute`, `prepare`) → `duckdb` API is similar

### Phase 1: Schema + CRUD Migration (2 days)

```rust
// db.rs: Replace rusqlite with duckdb
use duckdb::{params, Connection, Result};

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE SEQUENCE IF NOT EXISTS seq_delegation_id;
         CREATE TABLE IF NOT EXISTS sessions (...);
         CREATE TABLE IF NOT EXISTS delegation_log (
             id INTEGER PRIMARY KEY DEFAULT NEXTVAL('seq_delegation_id'),
             ...
         );
         CREATE TABLE IF NOT EXISTS daily_cubes (...);"
    )?;
    Ok(())
}
```

### Phase 2: Reporter Migration (1 day)

Replace `Reporter` that reads JSONL with one that queries DuckDB:
```rust
pub struct Reporter {
    conn: duckdb::Connection,
}

impl Reporter {
    pub fn daily_report(&self, range: ReportRange) -> Result<Vec<DailyReport>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                 strftime('%Y-%m-%d', timestamp) AS day,
                 agent, model, project,
                 SUM(input_tokens), SUM(output_tokens), SUM(cost_usd)
             FROM token_events
             WHERE timestamp BETWEEN ? AND ?
             GROUP BY day, agent, model, project
             ORDER BY day"
        )?;
        // ...
    }
}
```

### Phase 3: Ingestor Integration (1 day)

Ingestors write directly to DuckDB instead of producing `TokenEvent` structs for in-memory aggregation:
```rust
impl IngestorPipeline {
    pub fn ingest_to_duckdb(&self, conn: &duckdb::Connection) -> Result<usize> {
        let events = self.load_all()?;
        let appender = conn.appender("token_events")?;
        for event in events {
            appender.append_row(params![
                event.timestamp, event.session_id, event.agent,
                event.model, event.input_tokens, event.output_tokens,
                event.cost_usd
            ])?;
        }
        appender.flush()?;
        Ok(events.len())
    }
}
```

### Phase 4: Test Migration (1-2 days)

- Update 35 unit tests to use DuckDB in-memory connection
- Most tests are hermetic (tempfile or in-memory)
- SQL assertions are largely compatible

**Total migration effort: 5-7 days**

---

## Round 7: The Counter-Argument — Why Keep SQLite?

There are legitimate reasons to NOT migrate `spur-cost`:

### 7.1 Risk Aversion

- `spur-cost` works. It has 38 passing tests. It tracks costs accurately.
- Migrating it introduces risk of regressions in cost tracking.
- Cost tracking is critical — incorrect costs mean incorrect billing/projections.

### 7.2 Separation of Concerns

- `spur-cost` is an operational concern (dollars, tokens, time)
- `spur-context` is a semantic concern (decisions, observations, knowledge)
- Different concerns might warrant different storage technologies

### 7.3 SQLite's Robustness

- SQLite is the most tested database in the world (600+ TCLOC of tests)
- SQLite's ACID guarantees are bulletproof
- DuckDB is newer and less battle-tested for OLTP workloads
- If DuckDB crashes, `spur-cost` data is safe in SQLite

### 7.4 The Spec's Explicit Intent

The spec says:
> "`spur-context` is a new crate alongside `spur-cost`, not a replacement."
> "They complement rather than duplicate."

The spec authors explicitly chose separation. Overturning this requires strong justification.

---

## Synthesis: The Recommendation

### Short Term (Next 2 Weeks): Keep Separate, But Prepare

1. **Do not migrate `spur-cost` yet.** The approved spec works as designed.
2. **Add `spur-context` with DuckDB** as specified.
3. **Build the cross-database query abstraction:**
   ```rust
   pub struct UnifiedQuery {
       sqlite: rusqlite::Connection,
       duckdb: duckdb::Connection,
   }

   impl UnifiedQuery {
       /// Cross-database join: load cost data into DuckDB temp table
       pub fn attach_cost_data(&self) -> Result<()> {
           // Option A: DuckDB SQLite scanner
           self.duckdb.execute(
               "ATTACH '?1' AS cost (TYPE SQLITE)",
               params![self.sqlite_db_path]
           )?;
           Ok(())
       }
   }
   ```
4. **Measure friction:** Track how often developers need cross-database queries.

### Medium Term (After Phase 2 Validation): Evaluate Unification

**Trigger condition:** If any of these happen:
- More than 3 features require cross-database joins
- SQLite scanner performance is unacceptable (>500ms for simple joins)
- Developers complain about "which database do I query?"

**Action:** Schedule `spur-cost` → DuckDB migration (5-7 days).

### Long Term (After Phase 3): Unified DuckDB

**Trigger condition:** When the context engine is proven stable and the value of unified queries is validated.

**Action:**
1. Migrate `spur-cost` schema to DuckDB
2. Merge databases into single `context.db` (or rename to `spur.db`)
3. Remove `rusqlite` dependency
4. Update all queries to use native DuckDB SQL
5. Leverage DuckDB's Parquet export for long-term archival

---

## The Second-Order Insight

> **"The cost of two databases is not the sum of their individual costs. It's the cost of every cross-database query, every confused developer, and every feature that gets simplified because joining across databases is too hard."**

The approved spec made a reasonable choice: start separate, join via SQLite scanner. This is correct for Phase 1 when the context engine has 2 tables and no cross-domain queries.

But the spec's own Phase 3 query (Hybrid Graph RAG with RRF) implicitly assumes cost data is available in DuckDB:
```sql
-- From the spec:
FROM (SELECT *, ROW_NUMBER() OVER (ORDER BY score DESC) AS vector_rank FROM vector_hits) v
LEFT JOIN (SELECT *, ROW_NUMBER() OVER (ORDER BY weight DESC) AS graph_rank FROM graph_context) g
```

Nowhere in this query is there a join with cost data. But **there should be.** The most useful RAG ranking signal is not just "semantic similarity" but "semantic similarity × cost efficiency × success rate." A decision that was cheap, fast, and successful is more worth recalling than one that was expensive and failed.

The spec's architecture will eventually need cost data in DuckDB. The question is whether to:
- **Pay now:** Migrate cost to DuckDB (5-7 days, one-time)
- **Pay later:** Build workarounds (SQLite scanner, duplicated data, application joins) and migrate later anyway

**Second-order thinking says: pay now if Phase 2 is validated, pay later if Phase 1 fails.**

---

## Final Decision Matrix

| Criterion | Branch A: Separate | Branch B: Unified DuckDB | Branch C: Hybrid |
|-----------|-------------------|-------------------------|-----------------|
| Phase 1 effort | ★★★★★ (zero) | ★★☆☆☆ (5-7 days) | ★★★☆☆ (2-3 days) |
| Phase 2 query capability | ★★★☆☆ (workarounds) | ★★★★★ (native) | ★★★★☆ (duplicated data) |
| Phase 3 RAG quality | ★★☆☆☆ (limited) | ★★★★★ (full) | ★★★☆☆ (stale risk) |
| Data consistency | ★★★★★ (independent) | ★★★★★ (single source) | ★★☆☆☆ (dual write risk) |
| Build complexity | ★★★☆☆ (two C deps) | ★★★★☆ (one C++ dep) | ★★☆☆☆ (two C deps + sync) |
| Operational complexity | ★★★☆☆ (two files) | ★★★★★ (one file) | ★★☆☆☆ (two files + sync) |
| Risk of regression | ★★★★★ (none) | ★★★☆☆ (migration risk) | ★★★☆☆ (sync risk) |
| Long-term maintainability | ★★★☆☆ (friction) | ★★★★★ (clean) | ★★☆☆☆ (complex) |
| **Total** | **26** | **36** | **23** |

---

## The Concrete Plan

1. **Now:** Build `spur-context` as specified (DuckDB, separate from `spur-cost`)
2. **Phase 1 validation gate:** After `spur-context` is integrated and working, evaluate cross-database query friction
3. **Phase 2 prep:** If friction is high, schedule `spur-cost` → DuckDB migration before Phase 2 graph work
4. **Phase 2+:** Run unified DuckDB with native joins between cost, context, graph, and vector data

This defers the migration cost until we have evidence it's needed, but plans for it explicitly rather than letting technical debt accumulate.
