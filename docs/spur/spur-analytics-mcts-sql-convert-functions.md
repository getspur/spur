# MCTS + First Principles: SQL Convert Functions Per Agent

**Date:** 2026-04-23
**Status:** Architectural Evaluation (Staff Engineer Review)
**Trigger:** User insight — "Use SQL convert functions per agent. Schema changes? Just update the SQL."

---

## First Principles Grounding

### What Problem Are We Actually Solving?

SPUR needs to answer analytical questions across agent-generated JSONL files. The constraints:
1. **Source of truth is immutable**: Agent JSONL files are write-once, append-only logs
2. **Schemas are external**: Claude, Codex, Kiro own their JSONL formats; SPUR does not control them
3. **Schemas evolve**: When Claude Code updates, its JSONL schema may change
4. **Query interface must be unified**: Regardless of agent, the analytics layer sees one schema

### Core Principles

**P1: Data Gravity**
> "It's easier to bring compute to data than data to compute."
> — Jim Gray

The 2.14 GB of JSONL has gravity. Moving it through Rust memory → normalized files → DuckDB is fighting gravity. DuckDB reading JSONL in place respects data gravity.

**P2: Schema Volatility**
Agent JSONL schemas are external dependencies. They change unpredictably. The adaptation layer must be:
- Fast to change (minutes, not a release cycle)
- Isolated per agent (Claude changes don't affect Codex)
- Testable without recompilation

**P3: Complexity Localization**
Where should transformation complexity live?
- **SQL**: Declarative, schema-oriented, set-based. Good for field mapping, filtering, simple computation.
- **Rust**: Imperative, type-safe, exception-rich. Good for protocol logic, state machines, error recovery.
- **DuckDB**: Vectorized, parallel, analytical engine. Good for aggregations, window functions, joins.

The user's insight: schema transformation is fundamentally a **data shape problem**, not a **protocol logic problem**. SQL is the right language for data shape problems.

**P4: Minimal Moving Parts**
Every intermediate format is a failure mode:
- Rust parse error → normalized file corrupted
- Disk full → normalized write fails
- Race condition → two processes write normalized file
- Schema mismatch → normalized file has wrong shape

Direct JSONL → DuckDB eliminates the intermediate format.

---

## Round 1: MCTS — Pure Rust Ingestor (Current Path)

**Branch A: Rust Ingestor → Normalized JSONL/Parquet → DuckDB**

```
Agent JSONL
    │
    ▼
Rust Ingestor (per agent)
    ├── ClaudeIngestor::load_file() → Vec<TokenEvent>
    ├── CodexIngestor::load_file() → Vec<TokenEvent>  [with delta logic]
    └── KiroIngestor::load_file() → Vec<TokenEvent>
    │
    ▼
NormalizedWriter::write(events)
    ├── ~/.local/share/spur/normalized/claude/2026-04-23.jsonl
    ├── ~/.local/share/spur/normalized/codex/2026-04-23.jsonl
    └── ~/.local/share/spur/normalized/kiro/2026-04-23.jsonl
    │
    ▼
DuckDB: read_json_auto('normalized/**/*.jsonl')
```

**Strengths:**
- Full Rust type safety: `TokenEvent` is a struct with types
- Complex logic in Rust: Codex delta subtraction is clean imperative code
- Error handling: `serde_json` gives line-by-line error recovery
- Unit testable: Each ingestor has tests with fixture data

**Weaknesses:**
- Schema change requires Rust code change → recompile → release
- Intermediate write step: I/O overhead, disk usage, corruption risk
- Two sources of truth: JSONL + normalized files can diverge
- Live mode reads JSONL directly anyway (bypasses normalized files)

**Simulations:**

| Scenario | Outcome | Score |
|----------|---------|-------|
| Claude changes `costUSD` to `totalCost` | Update `ClaudeUsageEntry` struct, recompile, release | -0.3 |
| Codex adds new token field | Update `CodexIngestor`, handle backward compatibility | -0.2 |
| Disk full during normalized write | Partial file, corrupted data, manual cleanup | -0.4 |
| Live mode + batch report | Two code paths: Rust for live, DuckDB for batch | -0.3 |
| New agent (Gemini) joins | Write new Rust ingestor from scratch | -0.2 |
| **Total Score** | | **+0.2** |

**Verdict:** Works, but fights data gravity and schema volatility.

---

## Round 2: MCTS — DuckDB SQL Convert Functions (User Proposal)

**Branch B: DuckDB read_json_auto() → SQL Convert Views/Macros**

```
Agent JSONL (in place)
    │
    ├──► DuckDB: read_json_auto('~/.config/claude/**/*.jsonl')
    │    └──► SQL: CREATE VIEW claude_events AS SELECT ...
    │
    ├──► DuckDB: read_json_auto('~/.codex/sessions/**/*.jsonl')
    │    └──► SQL: CREATE VIEW codex_events AS SELECT ...
    │
    └──► DuckDB: read_json_auto('~/.kiro/sessions/**/*.jsonl')
         └──► SQL: CREATE VIEW kiro_events AS SELECT ...
    │
    ▼
DuckDB: CREATE VIEW all_events AS
        SELECT * FROM claude_events
        UNION ALL
        SELECT * FROM codex_events
        UNION ALL
        SELECT * FROM kiro_events
```

**Strengths:**
- Zero intermediate files: JSONL → DuckDB directly
- Schema change = SQL update: No recompile, no release
- DuckDB parses JSON: Vectorized, parallel, optimized
- Single code path: Live mode and batch mode both query DuckDB
- New agent = new SQL view: No Rust code needed

**Weaknesses:**
- Complex logic in SQL: Codex delta subtraction needs window functions
- Error handling: DuckDB `read_json_auto()` fails hard on malformed JSON
- Type safety: SQL is dynamically typed; schema mismatches fail at query time
- Pricing calculation: Need pricing data in DuckDB or compute elsewhere

**Simulations:**

| Scenario | Outcome | Score |
|----------|---------|-------|
| Claude changes `costUSD` to `totalCost` | Update `claude_events` SQL, reload views | +0.4 |
| Codex adds new token field | Update `codex_events` SQL, add column to view | +0.3 |
| Disk full | No intermediate files; query fails gracefully | +0.2 |
| Live mode + batch report | Both query same DuckDB views | +0.3 |
| New agent (Gemini) joins | Write new SQL view | +0.3 |
| Malformed JSON line | DuckDB aborts entire query | -0.3 |
| Codex delta logic in SQL | Complex window function, hard to debug | -0.2 |
| Pricing model missing | Query returns NULL cost | -0.2 |
| **Total Score** | | **+0.8** |

**Verdict:** Higher score. The user is right. Schema volatility is the dominant concern.

---

## Round 3: Deep Dive — What Can SQL Handle? What Needs Rust?

### Round 3A: Claude (Simple Case)

Claude JSONL schema (current):
```json
{
  "timestamp": "2026-04-23T10:30:00Z",
  "sessionId": "sess-abc-123",
  "costUSD": 0.05,
  "tokenUsage": {
    "inputTokens": 1000,
    "outputTokens": 500,
    "cacheReadTokens": 200,
    "cacheCreationTokens": 0
  },
  "model": "claude-sonnet-4-20250514",
  "project": "spur"
}
```

SQL convert function:
```sql
CREATE OR REPLACE VIEW claude_events AS
SELECT
    timestamp::TIMESTAMP AS timestamp,
    sessionId AS session_id,
    'claude' AS agent,
    NULLIF(model, '<synthetic>') AS model,
    project,
    tokenUsage.inputTokens AS input_tokens,
    tokenUsage.outputTokens AS output_tokens,
    tokenUsage.cacheReadTokens AS cache_read_tokens,
    tokenUsage.cacheCreationTokens AS cache_creation_tokens,
    costUSD AS cost_usd,
    filename() AS source_file
FROM read_json_auto(
    '~/.config/claude/projects/**/*.jsonl',
    columns = {
        timestamp: 'TIMESTAMP',
        sessionId: 'VARCHAR',
        costUSD: 'DOUBLE',
        tokenUsage: 'STRUCT(inputTokens BIGINT, outputTokens BIGINT, cacheReadTokens BIGINT, cacheCreationTokens BIGINT)',
        model: 'VARCHAR',
        project: 'VARCHAR'
    }
);
```

**Assessment:** ✅ Trivial in SQL. Field mapping, type casting, nested struct access. No Rust needed.

---

### Round 3B: Codex (Hard Case — Cumulative Delta)

Codex JSONL schema (simplified):
```json
{
  "timestamp": "2026-04-23T10:30:00Z",
  "session_id": "sess-def-456",
  "last_token_usage": {
    "input_tokens": 100,
    "output_tokens": 50,
    "cached_input_tokens": 20
  },
  "total_token_usage": {
    "input_tokens": 1100,
    "output_tokens": 550,
    "cached_input_tokens": 220
  },
  "model": "gpt-5"
}
```

The Rust logic:
1. Prefer `last_token_usage` (direct delta)
2. Fallback: `total_token_usage` − previous `total_token_usage` (cumulative delta)
3. `cached_input_tokens` → `cache_read_tokens`, capped at `input_tokens`

Can this be expressed in SQL?

```sql
CREATE OR REPLACE VIEW codex_events AS
WITH raw AS (
    SELECT
        timestamp::TIMESTAMP AS ts,
        session_id,
        model,
        last_token_usage.input_tokens AS last_input,
        last_token_usage.output_tokens AS last_output,
        last_token_usage.cached_input_tokens AS last_cached,
        total_token_usage.input_tokens AS total_input,
        total_token_usage.output_tokens AS total_output,
        total_token_usage.cached_input_tokens AS total_cached,
        filename() AS source_file
    FROM read_json_auto(
        '~/.codex/sessions/**/*.jsonl',
        columns = {
            timestamp: 'TIMESTAMP',
            session_id: 'VARCHAR',
            last_token_usage: 'STRUCT(input_tokens BIGINT, output_tokens BIGINT, cached_input_tokens BIGINT)',
            total_token_usage: 'STRUCT(input_tokens BIGINT, output_tokens BIGINT, cached_input_tokens BIGINT)',
            model: 'VARCHAR'
        }
    )
),
with_lag AS (
    SELECT *,
        LAG(total_input) OVER (PARTITION BY session_id ORDER BY ts) AS prev_total_input,
        LAG(total_output) OVER (PARTITION BY session_id ORDER BY ts) AS prev_total_output,
        LAG(total_cached) OVER (PARTITION BY session_id ORDER BY ts) AS prev_total_cached
    FROM raw
)
SELECT
    ts AS timestamp,
    session_id,
    'codex' AS agent,
    COALESCE(NULLIF(model, ''), 'gpt-5') AS model,
    -- Project extraction from path: e.g., ~/.codex/sessions/spur/2026-04-23.jsonl → 'spur'
    regexp_extract(source_file, 'sessions/([^/]+)/', 1) AS project,
    -- Delta logic: prefer last_token_usage, fallback to total delta
    COALESCE(last_input, total_input - COALESCE(prev_total_input, 0)) AS input_tokens,
    COALESCE(last_output, total_output - COALESCE(prev_total_output, 0)) AS output_tokens,
    -- cache_read_tokens = cached_input_tokens, capped at input_tokens
    LEAST(
        COALESCE(last_cached, total_cached - COALESCE(prev_total_cached, 0)),
        COALESCE(last_input, total_input - COALESCE(prev_total_input, 0))
    ) AS cache_read_tokens,
    0::BIGINT AS cache_creation_tokens,  -- Codex doesn't report this
    NULL::DOUBLE AS cost_usd,  -- Computed later via pricing join
    source_file
FROM with_lag;
```

**Assessment:** ⚠️ Possible but complex. The SQL is ~40 lines vs ~80 lines of Rust. But:
- Window functions handle the delta logic correctly
- `COALESCE` handles the fallback
- `LEAST` handles the cap
- The SQL is **declarative** — you say what you want, not how to compute it

**However:**
- If `last_token_usage` is missing for some rows and `total_token_usage` resets (new session), the `LAG` partition handles it
- But if the JSONL has a malformed line, `read_json_auto()` may fail the entire query
- The pricing join is separate (see below)

---

### Round 3C: Error Handling — The Weakness

**Rust approach:**
```rust
for line in reader.lines() {
    match serde_json::from_str::<ClaudeUsageEntry>(&line) {
        Ok(entry) => events.push(entry.into()),
        Err(e) => {
            tracing::warn!("Skipping malformed line {}: {}", line_num, e);
            continue;
        }
    }
}
```
→ Skips bad lines, continues processing, logs warnings.

**DuckDB approach:**
```sql
SELECT * FROM read_json_auto('*.jsonl')
```
→ If any line fails schema validation, the entire query fails.

**Mitigation 1: `ignore_errors=true`**
```sql
SELECT * FROM read_json_auto('*.jsonl', ignore_errors=true)
```
→ DuckDB skips malformed lines. But silently.

**Mitigation 2: `read_json` with `format='newline_delimited'` + `columns` spec**
```sql
SELECT * FROM read_json(
    '*.jsonl',
    format='newline_delimited',
    columns={...},
    maximum_depth=2,
    ignore_errors=true
)
```
→ More control, but still less granular than Rust.

**Mitigation 3: Hybrid — Rust pre-validates, DuckDB queries**
Have a lightweight Rust scan that validates JSONL and writes a "clean" list. DuckDB reads only clean files.
→ Adds complexity, defeats the purpose.

**Assessment:** Error handling is weaker in pure SQL. For SPUR's use case (analytics, not financial reporting), occasional skipped lines are acceptable. `ignore_errors=true` is sufficient.

---

### Round 3D: Pricing Calculation

The Rust `PricingRegistry` maps `(model, input_tokens, output_tokens, cache_read_tokens)` → `cost_usd`.

**Option 1: Load pricing into DuckDB**
```sql
CREATE TABLE pricing (
    model VARCHAR PRIMARY KEY,
    input_price_per_1m DOUBLE,    -- e.g., 3.0 for $3/1M tokens
    output_price_per_1m DOUBLE,
    cache_read_price_per_1m DOUBLE,
    cache_creation_price_per_1m DOUBLE
);

INSERT INTO pricing VALUES
    ('claude-sonnet-4-20250514', 3.0, 15.0, 0.3, 3.75),
    ('gpt-5', 2.0, 10.0, 0.5, 0.0);
```

Then compute cost in SQL:
```sql
SELECT
    e.*,
    (e.input_tokens * p.input_price_per_1m / 1e6)
    + (e.output_tokens * p.output_price_per_1m / 1e6)
    + (e.cache_read_tokens * p.cache_read_price_per_1m / 1e6)
    + (e.cache_creation_tokens * p.cache_creation_price_per_1m / 1e6)
    AS computed_cost_usd
FROM all_events e
LEFT JOIN pricing p ON p.model = e.model;
```

**Option 2: Keep pricing in Rust, pass to DuckDB**
```rust
// Load pricing registry
let pricing = load_pricing_registry();
// Serialize to DuckDB
conn.execute("CREATE TABLE pricing AS SELECT * FROM read_json_auto('pricing.json')")?;
```

**Option 3: Use agent-reported cost when available**
Claude reports `costUSD`. Codex does not. Use agent cost when present, compute when absent.

```sql
SELECT
    e.*,
    COALESCE(
        e.cost_usd,  -- agent-reported
        (e.input_tokens * p.input_price_per_1m / 1e6)
        + (e.output_tokens * p.output_price_per_1m / 1e6)
        + ...
    ) AS final_cost_usd
FROM all_events e
LEFT JOIN pricing p ON p.model = e.model;
```

**Assessment:** ✅ Pricing in DuckDB is clean. The `pricing` table is small (~50 rows). Updates are SQL `INSERT OR REPLACE`.

---

## Round 4: MCTS — Hybrid Approach (Rust Validation + SQL Transform)

**Branch C: Rust validates JSONL → DuckDB queries validated files**

```
Agent JSONL
    │
    ▼
Rust: Lightweight validator
    ├── Check each line is valid JSON
    ├── Check required fields exist
    ├── Skip malformed lines (log warnings)
    └── Write "clean manifest" (list of valid files)
    │
    ▼
DuckDB: read_json_auto(validated_files) → SQL convert views
```

**Simulations:**

| Scenario | Outcome | Score |
|----------|---------|-------|
| Malformed JSON | Rust skips line, DuckDB never sees it | +0.3 |
| Schema change | Update SQL only | +0.3 |
| Performance | Rust scan adds ~1s for 2GB | -0.1 |
| Complexity | Two systems to maintain | -0.2 |
| **Total Score** | | **+0.65** |

**Verdict:** Better error handling than pure SQL, but adds complexity. The user didn't ask for this — they want pure SQL. Given that `ignore_errors=true` handles 90% of cases, the complexity isn't justified.

---

## Round 5: Synthesis — The Winning Architecture

### The User Is Right

After 5 rounds of MCTS:

| Branch | Score | Verdict |
|--------|-------|---------|
| A: Rust → normalized → DuckDB | +0.2 | Rejected: fights data gravity |
| B: DuckDB SQL convert (pure) | +0.8 | **Accepted** |
| C: Rust validation + SQL | +0.65 | Rejected: unnecessary complexity |

The user's insight is correct: **SQL convert functions per agent** is the best approach.

### Why Branch B Wins

1. **Schema volatility dominates**: Agent JSONL schemas change. SQL is easier to update than Rust.
2. **Data gravity**: JSONL files are the source of truth. Don't move them.
3. **DuckDB is optimized for this**: Vectorized JSON parsing, parallel file scan, set-based transforms.
4. **Single path**: Live mode and batch mode both use DuckDB. No code path divergence.
5. **New agents are cheap**: Add a SQL view, no Rust code.

### The Concrete Design

```sql
-- ============================================
-- 1. PRICING TABLE (loaded from Rust registry)
-- ============================================
CREATE TABLE IF NOT EXISTS pricing (
    model VARCHAR PRIMARY KEY,
    input_price_per_1m DOUBLE,
    output_price_per_1m DOUBLE,
    cache_read_price_per_1m DOUBLE,
    cache_creation_price_per_1m DOUBLE,
    effective_from DATE,
    effective_to DATE
);

-- ============================================
-- 2. AGENT CONVERT VIEWS (one per agent)
-- ============================================

-- Claude: Simple field mapping
CREATE OR REPLACE VIEW claude_events AS
SELECT
    timestamp::TIMESTAMP AS timestamp,
    sessionId AS session_id,
    'claude' AS agent,
    NULLIF(model, '<synthetic>') AS model,
    project,
    tokenUsage.inputTokens AS input_tokens,
    tokenUsage.outputTokens AS output_tokens,
    tokenUsage.cacheReadTokens AS cache_read_tokens,
    tokenUsage.cacheCreationTokens AS cache_creation_tokens,
    costUSD AS cost_usd,
    filename() AS source_file
FROM read_json_auto(
    '~/.config/claude/projects/**/*.jsonl',
    ignore_errors=true
);

-- Codex: Cumulative delta via window functions
CREATE OR REPLACE VIEW codex_events AS
WITH raw AS (
    SELECT
        timestamp::TIMESTAMP AS ts,
        session_id,
        model,
        last_token_usage.input_tokens AS last_input,
        last_token_usage.output_tokens AS last_output,
        last_token_usage.cached_input_tokens AS last_cached,
        total_token_usage.input_tokens AS total_input,
        total_token_usage.output_tokens AS total_output,
        total_token_usage.cached_input_tokens AS total_cached,
        filename() AS source_file
    FROM read_json_auto(
        '~/.codex/sessions/**/*.jsonl',
        ignore_errors=true
    )
),
with_lag AS (
    SELECT *,
        LAG(total_input) OVER (PARTITION BY session_id ORDER BY ts) AS prev_input,
        LAG(total_output) OVER (PARTITION BY session_id ORDER BY ts) AS prev_output,
        LAG(total_cached) OVER (PARTITION BY session_id ORDER BY ts) AS prev_cached
    FROM raw
)
SELECT
    ts AS timestamp,
    session_id,
    'codex' AS agent,
    COALESCE(NULLIF(model, ''), 'gpt-5') AS model,
    regexp_extract(source_file, 'sessions/([^/]+)/', 1) AS project,
    COALESCE(last_input, total_input - COALESCE(prev_input, 0)) AS input_tokens,
    COALESCE(last_output, total_output - COALESCE(prev_output, 0)) AS output_tokens,
    LEAST(
        COALESCE(last_cached, total_cached - COALESCE(prev_cached, 0)),
        COALESCE(last_input, total_input - COALESCE(prev_input, 0))
    ) AS cache_read_tokens,
    0::BIGINT AS cache_creation_tokens,
    NULL::DOUBLE AS cost_usd,
    source_file
FROM with_lag;

-- Kiro: Stub (format TBD)
CREATE OR REPLACE VIEW kiro_events AS
SELECT
    NULL::TIMESTAMP AS timestamp,
    NULL::VARCHAR AS session_id,
    'kiro' AS agent,
    NULL::VARCHAR AS model,
    NULL::VARCHAR AS project,
    0::BIGINT AS input_tokens,
    0::BIGINT AS output_tokens,
    0::BIGINT AS cache_read_tokens,
    0::BIGINT AS cache_creation_tokens,
    NULL::DOUBLE AS cost_usd,
    NULL::VARCHAR AS source_file
WHERE FALSE;  -- Empty until format is documented

-- ============================================
-- 3. UNIFIED VIEW (all agents)
-- ============================================
CREATE OR REPLACE VIEW all_events AS
SELECT * FROM claude_events
UNION ALL
SELECT * FROM codex_events
UNION ALL
SELECT * FROM kiro_events;

-- ============================================
-- 4. COST-ENRICHED VIEW
-- ============================================
CREATE OR REPLACE VIEW all_events_with_cost AS
SELECT
    e.*,
    COALESCE(
        e.cost_usd,  -- Use agent-reported cost when available
        (e.input_tokens * p.input_price_per_1m / 1e6)
        + (e.output_tokens * p.output_price_per_1m / 1e6)
        + (e.cache_read_tokens * p.cache_read_price_per_1m / 1e6)
        + (e.cache_creation_tokens * p.cache_creation_price_per_1m / 1e6)
    ) AS computed_cost_usd
FROM all_events e
LEFT JOIN pricing p ON p.model = e.model
    AND e.timestamp >= p.effective_from
    AND (p.effective_to IS NULL OR e.timestamp < p.effective_to);
```

---

## Round 6: Live Mode with SQL Convert Functions

If all events are queryable via DuckDB views, live mode becomes:

```sql
-- Live snapshot for a specific session
SELECT
    session_id,
    agent,
    model,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(computed_cost_usd) AS cost_usd,
    MAX(timestamp) - MIN(timestamp) AS duration
FROM all_events_with_cost
WHERE session_id = 'sess-abc-123'
GROUP BY session_id, agent, model;
```

But this queries ALL historical JSONL. For live mode, we want to query only the active session's file.

**Optimization: Live mode queries only the active file**

```sql
-- For Claude live session
SELECT
    sessionId,
    MAX(costUSD) AS cost_usd,  -- Claude reports cumulative cost
    SUM(tokenUsage.inputTokens) AS input_tokens,
    SUM(tokenUsage.outputTokens) AS output_tokens
FROM read_json_auto('~/.config/claude/projects/spur/2026-04-23.jsonl')
WHERE sessionId = 'sess-abc-123';
```

Or, use a DuckDB macro:

```sql
CREATE MACRO live_session_cost(agent, session_id, file_glob) AS TABLE
    SELECT * FROM all_events_with_cost
    WHERE agent = agent AND session_id = session_id
      AND source_file LIKE file_glob;
```

**Assessment:** Live mode can use DuckDB directly, but for real-time updates (every 1-5 seconds), file polling is still needed. The SQL just replaces the aggregation logic.

---

## The Final Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    AGENT JSONL FILES (immutable)                 │
│                                                                  │
│   ~/.config/claude/projects/**/*.jsonl                           │
│   ~/.codex/sessions/**/*.jsonl                                   │
│   ~/.kiro/sessions/**/*.jsonl                                    │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                    DUCKDB ANALYTICS ENGINE                       │
│                                                                  │
│   SQL Convert Views (per agent):                                │
│   ├── claude_events:   field mapping, type casting              │
│   ├── codex_events:    window functions for delta               │
│   └── kiro_events:     (stub)                                   │
│                                                                  │
│   Unified Views:                                                 │
│   ├── all_events:      UNION ALL of all agents                  │
│   ├── all_events_with_cost: JOIN with pricing table             │
│                                                                  │
│   Analytics Queries:                                             │
│   ├── daily_report, weekly_report, monthly_report               │
│   ├── by_agent, by_model, by_project                            │
│   └── cross-domain:    JOIN with beads via SQLite scanner       │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                    LIVE MODE (Session Tracker)                   │
│                                                                  │
│   Active Session ──► Poll JSONL file ──► DuckDB query          │
│   (1-5s interval)       (incremental)     (SUM/MAX aggregation) │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                    OPERATIONAL (SQLite cost.db)                  │
│                                                                  │
│   Session lifecycle: start, end, status, agent, project         │
│   (unchanged — small, durable, transactional)                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan (SQL-First)

### Step 0: DuckDB Infrastructure (1 day)
- Add `duckdb` crate to workspace
- Verify `DUCKDB_DOWNLOAD_LIB=1` works in CI
- Create `spur-context` crate with DuckDB connection

### Step 1: SQL Convert Functions (2 days)
- Write `claude_events` SQL view
- Write `codex_events` SQL view (window functions for delta)
- Write `kiro_events` stub
- Write `pricing` table schema
- Write `all_events` and `all_events_with_cost` unified views

### Step 2: Rust-DuckDB Bridge (2 days)
- Load pricing registry from Rust into DuckDB `pricing` table
- Execute SQL views on DuckDB connection
- Expose query interface: `AnalyticsEngine::daily_report()`, etc.

### Step 3: Live Mode (1 day)
- File polling for active session JSONL
- DuckDB query for incremental aggregation
- TUI integration with auto-refresh

### Step 4: Reporter Refactor (2 days)
- Replace in-memory aggregation with DuckDB SQL queries
- Port daily/weekly/monthly reports to SQL
- Delete normalized JSONL writer (if it exists)

### Step 5: Testing (1 day)
- Test each SQL view with fixture data
- Test `all_events` UNION ALL
- Test pricing join and cost computation
- Test edge cases: missing fields, NULLs, schema variations

---

## The Staff Engineer's Principle Applied

> **"Schema transformation is a data shape problem, not a protocol logic problem. SQL is the right language for data shape problems."**

The Rust ingestors were solving the right problem (schema normalization) with the wrong tool (imperative code). By moving schema transformation to SQL:
- We eliminate intermediate formats
- We make schema changes trivial
- We leverage DuckDB's vectorized engine
- We keep Rust for what it does best: operational logic, ACP, orchestration

**The user's proposal is accepted. SQL convert functions per agent is the architecture.**
