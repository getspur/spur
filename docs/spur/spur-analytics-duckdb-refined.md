# SPUR Analytics: DuckDB First-Citizen with JSONL Direct Query + Live Mode

**Date:** 2026-04-23
**Status:** Refined Architecture (User Review)
**Principle:** DuckDB queries JSONL directly for analytics. SQLite tracks session lifecycle. Live mode accumulates cost on the fly.

---

## Refined Architecture

### What the User Said

1. **DuckDB reads JSONL directly** — Re-computing from JSONL via DuckDB is fast enough for analytics. No need for complex incremental SQLite ingestion.
2. **Live mode** — During an active session:
   - 2.1 Calculate the session cost
   - 2.2 Accumulate cost incrementally on the fly

### The Simplified Picture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        AGENT JSONL FILES                             │
│                                                                      │
│   ~/.config/claude/projects/**/*.jsonl    (Claude)                  │
│   ~/.codex/sessions/**/*.jsonl            (Codex)                   │
│   ~/.kiro/sessions/**/*.jsonl             (Kiro)                    │
│                                                                      │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │   spur-cost Ingestors (normalize to TokenEvent)              │   │
│   │   - ClaudeIngestor: sessionId, costUSD, model, tokens       │   │
│   │   - CodexIngestor:  cumulative delta, model                 │   │
│   │   - KiroIngestor:  (stub, format TBD)                       │   │
│   └─────────────────────────────────────────────────────────────┘   │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│                        OPERATIONAL PATH                              │
│                                                                      │
│   spur-core ──► CostTracker ──► SQLite cost.db                      │
│               (session lifecycle: start, end, status)               │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│                        ANALYTICS PATH (DuckDB)                       │
│                                                                      │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │   Option A: DuckDB read_json_auto()                          │   │
│   │   ──► Requires normalized schema (ingestor → Parquet/CSV)   │   │
│   │                                                             │   │
│   │   Option B: Ingestor → TokenEvent → DuckDB INSERT           │   │
│   │   ──► Full control, indexing, fast queries                  │   │
│   │                                                             │   │
│   │   Option C: Ingestor → normalized JSONL → DuckDB read_json  │   │
│   │   ──► Best of both: native DuckDB + agent schema handling   │   │
│   └─────────────────────────────────────────────────────────────┘   │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│                        LIVE MODE (On-The-Fly)                        │
│                                                                      │
│   Active Session ──► Poll JSONL ──► Ingestor ──► Running Totals    │
│                                                                      │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │   Session: claude-2026-04-23-abc123                          │   │
│   │   Status: running                                            │   │
│   │   File: ~/.config/claude/projects/spur/2026-04-23.jsonl      │   │
│   │                                                             │   │
│   │   Poll every N seconds ──► parse new lines ──► accumulate   │   │
│   │                                                             │   │
│   │   Running:                                                   │   │
│   │   - input_tokens: 12,345                                     │   │
│   │   - output_tokens: 8,901                                     │   │
│   │   - cost_usd: $0.42                                          │   │
│   │   - duration: 00:04:32                                       │   │
│   └─────────────────────────────────────────────────────────────┘   │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## The Two Paths Clarified

### Path 1: Operational (SQLite `cost.db`)

**What it tracks:** Session metadata — start time, end time, status, agent name, project.

**Why SQLite:** Simple, durable, embedded. Low write volume.

```rust
// Called by orchestrator
impl CostTracker {
    fn start_session(&self, id, agent, project) -> Result<()>;
    fn end_session(&self, id) -> Result<()>;
    fn update_session_status(&self, id, status) -> Result<()>;
}
```

**Does NOT track:** Token counts, model, cost. That lives in the JSONL.

### Path 2: Analytics (DuckDB)

**What it queries:** Token usage, cost, model breakdown — computed from JSONL.

**Why DuckDB:** Fast columnar scans, `read_json_auto()`, `read_parquet()`, vectorized aggregation.

```sql
-- DuckDB can read JSONL directly IF schema is consistent
SELECT
    sessionId,
    SUM(totalInputTokens) AS input_tokens,
    SUM(totalOutputTokens) AS output_tokens,
    MAX(costUSD) AS cost_usd
FROM read_json_auto('~/.config/claude/projects/**/*.jsonl')
GROUP BY sessionId;
```

**Problem:** Each agent has a different JSONL schema. DuckDB `read_json_auto()` assumes consistent schema.

### The Bridge: Normalized JSONL

Instead of DuckDB reading raw agent JSONL directly, the ingestor normalizes to a unified schema and writes a normalized JSONL/Parquet that DuckDB reads:

```
Agent JSONL (schema varies)
    │
    ▼
Ingestor (ClaudeIngestor, CodexIngestor, etc.)
    │
    ▼
Normalized format (consistent schema)
    │
    ├──► Option C1: Write to `~/.local/share/spur/normalized/*.jsonl`
    │     DuckDB: `read_json_auto('~/.local/share/spur/normalized/*.jsonl')`
    │
    ├──► Option C2: Write to `~/.local/share/spur/normalized/sessions.parquet`
    │     DuckDB: `SELECT * FROM read_parquet('sessions.parquet')`
    │
    └──► Option C3: Insert directly into DuckDB
          DuckDB: `INSERT INTO sessions (...) VALUES (...)`
```

---

## Live Mode Design

### What Is Live Mode?

During an active session (e.g., Kimi Code CLI is running), SPUR shows:
- Current session running cost
- Token accumulation rate
- Estimated total cost

### How It Works

```rust
pub struct LiveSessionTracker {
    session_id: String,
    source_file: PathBuf,
    ingestor: Box<dyn Ingestor>,

    // Running totals
    last_line_count: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cost_usd: f64,

    // Poll interval
    poll_interval: Duration,
}

impl LiveSessionTracker {
    /// Start tracking a live session
    pub fn start(session_id: &str, source_file: &Path, ingestor: Box<dyn Ingestor>) -> Self {
        Self {
            session_id: session_id.to_string(),
            source_file: source_file.to_path_buf(),
            ingestor,
            last_line_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
            poll_interval: Duration::from_secs(5),
        }
    }

    /// Poll for new data and update running totals
    pub fn poll(&mut self) -> Result<LiveSessionSnapshot> {
        // 1. Read file, skip already-parsed lines
        let file = File::open(&self.source_file)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines()
            .skip(self.last_line_count)
            .collect::<Result<Vec<_>, _>>()?;

        if lines.is_empty() {
            return Ok(self.to_snapshot());
        }

        // 2. Parse new lines via ingestor
        //    For Claude: each line is a complete entry
        //    For Codex: each line is cumulative, need delta
        let new_events = self.ingestor.parse_lines(&lines)?;

        // 3. Accumulate
        for event in &new_events {
            self.input_tokens += event.input_tokens;
            self.output_tokens += event.output_tokens;
            self.cache_read_tokens += event.cache_read_tokens;
            self.cache_creation_tokens += event.cache_creation_tokens;
            if let Some(cost) = event.cost_usd {
                self.cost_usd += cost;  // Or replace with latest? Depends on schema
            }
        }

        self.last_line_count += lines.len();

        Ok(self.to_snapshot())
    }

    fn to_snapshot(&self) -> LiveSessionSnapshot {
        LiveSessionSnapshot {
            session_id: self.session_id.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.input_tokens + self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cost_usd: self.cost_usd,
            estimated_total_cost: self.cost_usd * 1.2, // heuristic
        }
    }
}
```

### Live Mode in TUI

```
┌─ Live Session: claude-2026-04-23-abc123 ──────────────────────────┐
│ Status:   🟢 Running (00:04:32)                                   │
│ Agent:    Claude Code                                             │
│ Project:  spur                                                    │
│                                                                    │
│ Tokens:   ▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░  21,246 / ~30,000  (est)         │
│   Input:  12,345                                                  │
│   Output: 8,901                                                   │
│   Cache:  2,100 read, 0 creation                                  │
│                                                                    │
│ Cost:     $0.42 / ~$0.60 (est)                                    │
│ Rate:     $0.09/min                                               │
│                                                                    │
│ Model:    claude-sonnet-4-20250514                                │
│ Source:   ~/.config/claude/projects/spur/2026-04-23.jsonl         │
│                                                                    │
│ [Press 'q' to quit live view]  [Press 's' to stop session]       │
└────────────────────────────────────────────────────────────────────┘
```

---

## The Analytics Query Layer

### For Historical Reports (Batch)

DuckDB reads normalized data (JSONL or Parquet) and computes aggregations:

```sql
-- Daily cost breakdown by agent
SELECT
    strftime(timestamp, '%Y-%m-%d') AS day,
    agent,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cost_usd) AS cost_usd,
    COUNT(DISTINCT session_id) AS sessions
FROM read_json_auto('~/.local/share/spur/normalized/**/*.jsonl')
WHERE timestamp > now() - INTERVAL '30 days'
GROUP BY day, agent
ORDER BY day DESC, cost_usd DESC;

-- Model cost comparison
SELECT
    model,
    COUNT(*) AS requests,
    AVG(cost_usd) AS avg_cost,
    SUM(cost_usd) AS total_cost,
    SUM(input_tokens) / SUM(duration_seconds) AS input_tps
FROM read_json_auto('~/.local/share/spur/normalized/**/*.jsonl')
WHERE model IS NOT NULL
GROUP BY model
ORDER BY total_cost DESC;

-- Project cost over time
SELECT
    project,
    strftime(timestamp, '%Y-%m') AS month,
    SUM(cost_usd) AS cost,
    COUNT(DISTINCT session_id) AS sessions
FROM read_json_auto('~/.local/share/spur/normalized/**/*.jsonl')
WHERE project IS NOT NULL
GROUP BY project, month
ORDER BY project, month;
```

### For Cross-Domain Queries (with beads)

```sql
-- Cost per issue (join with beads via DuckDB SQLite scanner)
ATTACH '.beads/beads.db' AS beads (TYPE SQLITE);

SELECT
    i.title,
    i.status,
    SUM(n.cost_usd) AS total_cost,
    COUNT(DISTINCT n.session_id) AS sessions
FROM beads.issues i
LEFT JOIN read_json_auto('~/.local/share/spur/normalized/**/*.jsonl') n
    ON n.session_id = i.external_ref
GROUP BY i.id, i.title, i.status
ORDER BY total_cost DESC;
```

---

## Implementation Questions

Before implementing, I need to clarify:

### Q1: Live Mode — Where Does the Cost Come From?

| Option | Source | Pros | Cons |
|--------|--------|------|------|
| A | Parse JSONL file as it grows | Accurate, no agent changes needed | I/O overhead, file locking issues |
| B | Agent reports to SPUR via ACP/MCP | Real-time, no file I/O | Requires agent integration |
| C | Hybrid: Agent reports + file fallback | Robust | More complex |

### Q2: Normalized Format — JSONL or Parquet?

| Format | Pros | Cons |
|--------|------|------|
| JSONL | Human-readable, append-friendly | Slower scans, larger |
| Parquet | Fast columnar, small | Not append-friendly (need rewrite) |
| Arrow/IPC | Fast in-memory, zero-copy | DuckDB support varies |

**Recommendation:** Write normalized JSONL (append-friendly), let DuckDB read it. Periodically compact to Parquet for faster historical queries.

### Q3: Session Lifecycle vs Token Data Split

Currently:
- `CostTracker` (SQLite) tracks: session_id, agent, project, start_time, end_time, status
- Reporter (JSONL) tracks: tokens, model, cost, timestamps

**Should SQLite also store token totals?** Or should the JSONL be the sole source of truth for token/cost data?

| Approach | Pros | Cons |
|----------|------|------|
| JSONL only | Single source of truth for tokens/cost | SQLite can't query cost without DuckDB |
| JSONL + SQLite summary | SQLite has fast lookups, DuckDB has analytics | Data duplication, sync issues |

**Recommendation:** JSONL is sole source of truth for tokens/cost. SQLite stores session metadata only. For cost lookups, use DuckDB or cache in memory.

---

## The Concrete Plan (Refined)

### Step 1: Live Mode Ingestor (2 days)

1. Add `LiveSessionTracker` to `spur-cost`
2. Implement incremental JSONL parsing (skip already-read lines)
3. Add `poll()` method that returns `LiveSessionSnapshot`
4. Wire into TUI with auto-refresh

### Step 2: Normalized JSONL Writer (1 day)

1. Add `NormalizedWriter` that writes `TokenEvent` → `~/.local/share/spur/normalized/{agent}/{date}.jsonl`
2. Run on demand or after session end
3. Unified schema: `timestamp, session_id, agent, model, project, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cost_usd, source_file`

### Step 3: DuckDB Analytics Queries (2 days)

1. Add `AnalyticsEngine` in `spur-context` (or `spur-cost`)
2. `read_json_auto()` on normalized JSONL
3. Pre-built queries: daily, weekly, monthly, by agent, by model, by project
4. Cross-domain: join with beads via SQLite scanner

### Step 4: Reporter Refactor (2 days)

1. Replace in-memory aggregation with DuckDB queries
2. Cache query results for fast TUI rendering
3. Support live mode snapshot integration

---

## The Core Insight

> **"Operational state = SQLite (small, durable). Analytics = DuckDB (fast, flexible). Token data = JSONL files (immutable, auditable). Live mode = incremental file parsing (simple, accurate)."**

This is simpler than the previous architecture because:
1. ✅ No incremental SQLite ingestion tables
2. ✅ No materialized views to refresh
3. ✅ No data duplication between SQLite and JSONL
4. ✅ DuckDB reads normalized JSONL directly (what it's best at)
5. ✅ Live mode is just incremental file reading

**The trade-off:** Reports re-compute from JSONL each time. But with DuckDB + columnar Parquet (if we compact), this is fast enough for SPUR's scale.
