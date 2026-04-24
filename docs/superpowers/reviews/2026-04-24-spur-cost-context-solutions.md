# Solutions Design — `spur-cost` / `spur-context` Findings

- **Date:** 2026-04-24
- **Reviewer:** L9 Rust / data-engineering staff review
- **Predecessors:**
  - Code-level review — `docs/superpowers/reviews/2026-04-24-spur-cost-spur-context-l9-review.md`
  - Self-test verdict — `docs/superpowers/reviews/2026-04-24-spur-cost-self-test-verdict.md`
- **This doc:** concrete solutions, each empirically grounded or reasoned from first principles. Per-finding remediation + test plan + dependencies.
- **Review pass (v2, 2026-04-24):** first-principles review by kimi surfaced three concrete concerns (R1 needs a dash-boundary guard; 1A needs CRLF handling; 2A's in-memory engine open per invocation is unusable at 1.8 GB). All three incorporated below. New **Phase 2.5** added for persistent cache + incremental refresh — gates the user-facing ship.

## TL;DR

All 10 findings collapse to **four root causes**:

1. **Schema-inference vs. heterogeneous JSONL** (CRIT-1, HIGH-3, MED-3) — fix by replacing `read_json_auto` with raw-text + `json_extract`. Empirically verified: parses 492,524 real Claude rows where the current code fails at CREATE VIEW time.
2. **Integration gap between CLI, engine, and orchestrator** (CRIT-2, HIGH-1) — wire `spur cost --engine duckdb` (Phase 2), which resolves HIGH-1 as a side-effect because DuckDB reads tokens directly from JSONL.
3. **UX correctness in the presentation layer** (HIGH-2, MED-1, LOW-1) — range-scoped breakdowns, quiet default log level, normalize `foo-acp → foo` at write time.
4. **Defensive-programming details** (MED-2, LOW-2, C3-regression) — shared path resolver, `walkdir`-based discovery, deterministic longest-prefix match in `PricingRegistry`.

**Revised verdict on the current `fix/l9-review-criticals` branch:**

- **C3 has a regression.** Real Claude model slug is `claude-haiku-4-5-20251001`; registry has `claude-haiku-4`. The old (nondeterministic) substring fallback happened to match via the `key.contains(k)` branch. My C3 fix removed it entirely. Net effect: Haiku 4.5 sessions now get `None` and fall to time-tier. **Before merging C3, add a deterministic longest-prefix match.** See Fix R1 below.
- **C1 and C2 do not work on real data.** Even with my rewrite. DuckDB's schema inference over heterogeneous Claude JSONL rejects projected columns. **Revert C1 and C2.** Replace with Fix 1A.

Minimum path to ship corrected cost to users: Phases 0 → 1 → 2 → **2.5** + MED-1 from Phase 3. Phase 2 without 2.5 is a prototype, not a user-facing feature (rescans 1.8 GB per CLI invocation).

---

## Root Cause 1 — Schema inference vs. heterogeneous JSONL

### Fix 1A — `json_extract` over raw-text JSONL (resolves CRIT-1, HIGH-3, MED-3)

**Root cause:** `read_json_auto` is designed for homogeneous record sets. Real Claude JSONL mixes `user`/`assistant`/`attachment`/`tool_result`/`queue-operation`/`last-prompt` row types in one file. DuckDB's schema inference samples some rows and can't resolve field references (`costUSD`, `message.usage.*`) that don't exist on the sampled rows. `union_by_name=true` and `sample_size=-1` do not rescue this.

**Empirical proof that `json_extract` works:** Running against the user's real `~/.claude/projects/**/*.jsonl` (4110 files, 1.8 GB):

```sql
-- Step 1: read as VARCHAR (one JSONL line per row). NUL delimiter because
-- no ASCII NUL occurs inside JSONL. rtrim(chr(13)) strips any trailing CR
-- on CRLF-terminated files (Windows-authored JSONL). Records are still
-- split on \n regardless of delim; only field-splitting uses delim.
CREATE OR REPLACE VIEW claude_raw AS
SELECT
    rtrim(line, chr(13)) AS line,
    filename
FROM read_csv_auto(
    '/path/to/.claude/projects/**/*.jsonl',
    columns = {'line': 'VARCHAR'},
    delim = '\0',
    header = false,
    filename = true,
    ignore_errors = true,
    quote = '',
    escape = ''
);

-- Step 2: extract per-row, deriving semantic columns.
CREATE OR REPLACE VIEW claude_events AS
SELECT
    TRY_CAST(json_extract_string(line, '$.timestamp') AS TIMESTAMP) AS timestamp,
    json_extract_string(line, '$.sessionId') AS session_id,
    'claude' AS agent,
    NULLIF(json_extract_string(line, '$.message.model'), '<synthetic>') AS model,
    NULLIF(
        regexp_extract(filename, '.*/projects/([^/]+)/.*[.]jsonl$', 1),
        ''
    ) AS project,
    TRY_CAST(json_extract(line, '$.message.usage.input_tokens') AS BIGINT) AS input_tokens,
    TRY_CAST(json_extract(line, '$.message.usage.output_tokens') AS BIGINT) AS output_tokens,
    TRY_CAST(json_extract(line, '$.message.usage.cache_read_input_tokens') AS BIGINT) AS cache_read_tokens,
    TRY_CAST(json_extract(line, '$.message.usage.cache_creation_input_tokens') AS BIGINT) AS cache_creation_tokens,
    TRY_CAST(json_extract(line, '$.costUSD') AS DOUBLE) AS cost_usd
FROM claude_raw
WHERE json_extract_string(line, '$.type') = 'assistant';
```

Empirical result: 492,524 rows parsed; sampled rows correctly return `type='assistant'`, nested tokens, model slug. One malformed unicode row broke my probe's `SUM(...)` — `TRY_CAST` above defuses it (aggregate skips NULLs).

**Codex view — same pattern:**

```sql
CREATE OR REPLACE VIEW codex_raw AS
SELECT
    rtrim(line, chr(13)) AS line,
    filename
FROM read_csv_auto(
    '/path/to/.codex/sessions/**/*.jsonl',
    columns = {'line': 'VARCHAR'},
    delim = '\0', header = false, filename = true,
    ignore_errors = true, quote = '', escape = ''
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
FROM codex_raw;

CREATE OR REPLACE VIEW codex_events AS
WITH with_carried_model AS (
    SELECT *,
        LAST_VALUE(COALESCE(event_model, turn_model) IGNORE NULLS) OVER (
            PARTITION BY filename ORDER BY ts
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS current_model
    FROM codex_token_events
    WHERE ts IS NOT NULL
),
with_delta AS (
    SELECT *,
        COALESCE(
            last_in,
            GREATEST(tot_in - COALESCE(LAG(tot_in) OVER (PARTITION BY session_id ORDER BY ts), 0), 0::BIGINT)
        ) AS input_delta,
        COALESCE(
            last_out,
            GREATEST(tot_out - COALESCE(LAG(tot_out) OVER (PARTITION BY session_id ORDER BY ts), 0), 0::BIGINT)
        ) AS output_delta,
        COALESCE(
            last_cached,
            GREATEST(tot_cached - COALESCE(LAG(tot_cached) OVER (PARTITION BY session_id ORDER BY ts), 0), 0::BIGINT)
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
    input_delta AS input_tokens,
    output_delta AS output_tokens,
    LEAST(cached_delta, input_delta) AS cache_read_tokens,
    0::BIGINT AS cache_creation_tokens,
    NULL::DOUBLE AS cost_usd
FROM with_delta
WHERE input_delta > 0 OR output_delta > 0 OR cached_delta > 0;
```

**Kiro view:** empty stub retained (Kiro tokens flow through ACP, not the filesystem).

**Test plan:**
1. Commit two real-shape, anonymized fixtures under `crates/spur-context/tests/fixtures/real/`:
   - `claude_heterogeneous.jsonl` — 1 each of user / assistant (with usage) / attachment / tool_result / queue-operation rows. Small (< 2 KB).
   - `codex_heterogeneous.jsonl` — turn_context / event_msg(token_count direct) / event_msg(token_count cumulative only) / response_item / session_meta.
2. Integration test `tests/real_fixtures.rs`:
   - `AnalyticsEngine::open_in_memory().initialize().create_agent_views()` pointing env vars at the fixtures dir.
   - Assert view row counts > 0.
   - Assert specific token deltas, session_id = filename stem, model = `claude-sonnet-4` / `gpt-5`.
3. The inline unit tests in `engine.rs` stay (they test SQL syntax + delta logic in isolation).

**Effort:** ~200 LOC of SQL + 2 fixture files + 1 integration test. One medium PR.

**Dependency:** Must land before any other fix in this root cause; everything else in spur-context sits on top of the view.

---

## Root Cause 2 — Integration gap

### Fix 2A — Wire `spur cost --engine duckdb` (resolves CRIT-2; sideresolves HIGH-1)

**Root cause:** The CLI only talks to `CostTracker` (SQLite). `spur-context` is a library with no binary-level entry point. Any improvement to the analytics engine is unfalsifiable by users.

**Fix sketch:**

```rust
// crates/spur-cli/src/main.rs — Cost subcommand
#[derive(clap::ValueEnum, Clone, Debug)]
enum CostEngine { Sqlite, Duckdb }

Commands::Cost { week, by, export, engine, range, .. } => {
    match engine.unwrap_or(CostEngine::Sqlite) {
        CostEngine::Sqlite => /* existing path */,
        CostEngine::Duckdb => {
            use spur_context::{AnalyticsEngine, Reporter, ReportRange};
            let engine = AnalyticsEngine::open_in_memory()?;
            engine.initialize()?;
            engine.create_agent_views()?;
            engine.load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())?;
            let reporter = Reporter::new(engine);
            let range = match (week, range.as_deref()) {
                (true, _) => ReportRange::last_days(7),
                (_, Some("today")) | (_, None) => ReportRange::today(),
                (_, Some(s)) => /* parse YYYY-MM-DD..YYYY-MM-DD */,
            };
            let reports = reporter.daily_report(range)?;
            render_reports(&reports, export.as_deref(), by.as_deref());
        }
    }
}
```

**Why this also resolves HIGH-1 (orchestrator never calls `end_session_with_tokens`):**
Once DuckDB reads tokens directly from JSONL, the orchestrator does not need to populate them. The agent already wrote the authoritative token data to its own JSONL. The SQLite `sessions.{input,output,...}_tokens` columns become optional metadata for live sessions only.

**Test plan:**
- E2E: with env vars pointing at fixture dir, `spur cost --engine duckdb --range 2026-04-20..2026-04-25` shows non-zero cost for the fixture's assistant rows.
- Compare output shape against `--engine sqlite` for the same window: both produce a table; DuckDB shows richer token columns.

**Effort:** ~120 LOC in `spur-cli` + reuse of existing `Reporter` / `AnalyticsEngine`. Small PR.

**Dependency:** Requires Fix 1A (otherwise users see zero rows from the real JSONL).

**⚠️ Constraint: this sketch is a prototype, not a shippable CLI.**
`open_in_memory()` rebuilds all views and rescans the full JSONL globs on every
invocation. Against the user's 1.8 GB Claude dataset that is multi-second
latency for a command users expect to respond in under a second. **Do not ship
Phase 2 without Phase 2.5** (persistent cache + incremental refresh) — ship
2A behind a `--experimental` flag if you need to validate the SQL in users'
hands before 2.5 lands, but don't make it the default.

### Fix 2.5 — Persistent DuckDB cache + incremental refresh (gates user-facing ship)

**Root cause:** Phase 2's `open_in_memory()` means every CLI invocation
re-reads 1.8 GB of JSONL. Interactive CLI UX requires sub-second response;
this design cannot deliver it.

**Fix sketch:**

```rust
// crates/spur-context/src/engine.rs (additive)
impl AnalyticsEngine {
    /// Open the persistent analytics cache. Creates the file + base schema
    /// on first use. Safe to re-open across processes (read lock only).
    pub fn open_cache(cache_path: &Path) -> Result<Self> {
        let conn = Connection::open(cache_path)?;
        conn.execute_batch(CACHE_SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Incrementally refresh the cache: for each JSONL file newer than its
    /// recorded `loaded_at`, DELETE existing rows for that path and re-insert.
    /// For a file with unchanged mtime, skip.
    pub fn refresh_incremental(&self) -> Result<RefreshStats> {
        let mut stats = RefreshStats::default();
        for path in discover_all_agent_jsonl()? {
            let mtime = fs::metadata(&path)?.modified()?;
            let loaded_at: Option<SystemTime> = self.conn.query_row(
                "SELECT loaded_at FROM scan_manifest WHERE path = ?",
                [path.to_string_lossy()],
                |r| r.get(0),
            ).optional()?;
            if loaded_at.map_or(true, |t| mtime > t) {
                self.reload_file(&path)?;  // DELETE WHERE filename=? + INSERT
                self.conn.execute(
                    "INSERT INTO scan_manifest(path, mtime, loaded_at)
                     VALUES (?, ?, now()) ON CONFLICT(path) DO UPDATE
                     SET mtime=excluded.mtime, loaded_at=excluded.loaded_at",
                    [path.to_string_lossy(), mtime_iso(&mtime)],
                )?;
                stats.reloaded += 1;
            } else {
                stats.skipped += 1;
            }
        }
        Ok(stats)
    }
}
```

**Schema additions** (`src/sql/schema.sql`):
```sql
CREATE TABLE IF NOT EXISTS scan_manifest (
    path      VARCHAR PRIMARY KEY,
    mtime     TIMESTAMP NOT NULL,
    loaded_at TIMESTAMP NOT NULL
);

-- events_cache shadows the extracted columns from claude_raw/codex_raw,
-- eliminating the per-query json_extract cost after first load.
CREATE TABLE IF NOT EXISTS events_cache (
    filename        VARCHAR NOT NULL,
    timestamp       TIMESTAMP,
    session_id      VARCHAR,
    agent           VARCHAR,
    model           VARCHAR,
    project         VARCHAR,
    input_tokens    BIGINT,
    output_tokens   BIGINT,
    cache_read_tokens     BIGINT,
    cache_creation_tokens BIGINT,
    cost_usd        DOUBLE
);
CREATE INDEX IF NOT EXISTS idx_events_cache_ts ON events_cache(timestamp);
CREATE INDEX IF NOT EXISTS idx_events_cache_filename ON events_cache(filename);

CREATE OR REPLACE VIEW all_events_with_cost AS
SELECT e.*,
  COALESCE(e.cost_usd, /* token × pricing fallback */ ) AS computed_cost_usd
FROM events_cache e LEFT JOIN pricing p
  ON /* same as before */;
```

**CLI integration** (updates Fix 2A):
```rust
let cache_dir = repo_root.join(".spur/cache");
std::fs::create_dir_all(&cache_dir)?;
let engine = AnalyticsEngine::open_cache(&cache_dir.join("cost.duckdb"))?;
let stats = engine.refresh_incremental()?;
tracing::debug!(?stats, "incremental cache refresh");
let reporter = Reporter::new(engine);
// ... render as before; now sub-second after first warm-up.
```

**Properties:**
- Cold first run: still O(all JSONL) — unavoidable.
- Subsequent runs: O(changed files) — typically zero in an idle session.
- Crash safety: mtime check is idempotent; a partial INSERT is harmless because the next invocation re-reloads the file.
- Multi-process: DuckDB's WAL mode permits concurrent readers; only one writer at a time (OK — CLI invocations are short).

**Test plan:**
- Unit: load fixture with N rows; call `refresh_incremental()`; assert N rows in `events_cache`. Touch the fixture (bump mtime); re-run; assert N rows still (not 2N — reload is DELETE-then-INSERT by filename).
- E2E: measure `spur cost --engine duckdb` cold vs warm. Target warm < 200 ms on 1.8 GB corpus.

**Effort:** ~250 LOC (schema, refresh logic, integration test, benchmark). One medium PR.

**Dependency:** Fix 1A provides the extraction SQL that `reload_file` runs per file.

### Fix 2E (follow-up) — Retire SQLite cost reads once 2A+2.5 are stable

- Keep `CostTracker` for session lifecycle writes (start/end, status).
- Drop `today_summary`, `week_summary`, `by_project`, `by_model`, `today_token_summary`, `week_token_summary`, `query_cost_today`, `query_cost_range`, `query_cost_by_model`, `query_cost_by_project` from the public API.
- `spur cost` default engine switches from `sqlite` to `duckdb`. Remove `--engine sqlite` two releases later.

**Dependency:** After 2A ships and is verified in user hands for one release cycle.

---

## Root Cause 3 — UX correctness

### Fix 3A — Range-scoped breakdowns (resolves HIGH-2)

**Root cause:** `Commands::Cost { by: Some("project") }` calls `ct.by_project()` (all-time), regardless of whether the primary table is today or `--week`.

**Fix:**
```rust
// crates/spur-cost/src/tracker.rs
pub fn by_project_range(&self, range: ReportRange) -> Result<Vec<ProjectCostSummary>> { ... }
pub fn by_model_range(&self, range: ReportRange) -> Result<Vec<ModelCostSummary>> { ... }

// crates/spur-cli/src/main.rs
if let Some(ref dim) = by {
    let label = if week { "Last 7 days" } else { "Today" };
    match dim.as_str() {
        "project" => {
            println!("\nBy project ({}):", label);
            for p in ct.by_project_range(range)? { ... }
        }
        "model" => {
            println!("\nBy model ({}):", label);
            for m in ct.by_model_range(range)? { ... }
        }
        other => eprintln!("Unknown --by dimension: {}", other),
    }
}
```

**Test plan:** fixture test — load 3 sessions today + 3 sessions last week into an in-memory SQLite → `spur cost --by project` totals match today only; `spur cost --week --by project` totals match all 6.

**Effort:** ~40 LOC.

### Fix 3B — Quiet default log level (resolves MED-1)

**Root cause:** `tracing_subscriber::fmt().init()` with no `EnvFilter`; agent-registry emits INFO for each agent on every CLI invocation.

**Fix in `crates/spur-cli/src/main.rs` entrypoint:**
```rust
use tracing_subscriber::EnvFilter;

let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new(
        match std::env::args().any(|a| a == "--verbose" || a == "-v") {
            true  => "info",
            false => "warn,spur_acp::agents::defaults=warn",
        }
    ));
tracing_subscriber::fmt()
    .with_env_filter(filter)
    .with_writer(std::io::stderr)
    .init();
```

Separately, downgrade the `spur_acp::agents::defaults` per-agent registration logs from `info` to `debug`. They fire on every invocation and carry no user-useful signal in the normal path.

**Test plan:** manual — `spur cost` emits only the table; `RUST_LOG=info spur cost` emits the full log trace.

**Effort:** ~20 LOC + downgrade 3 `tracing::info!` call sites in `spur_acp::agents::defaults` to `debug!`.

### Fix 3C — Normalize agent name at write (resolves LOW-1)

**Root cause:** `orchestrator.rs:959, 1852, 1960` all pass the raw registered agent name into `CostTracker::end_session`. Registered names include the `-acp` suffix; cost rows split on it. `normalize_agent_name()` already exists in the same file but is only used for routing comparison.

**Fix:**
```rust
// crates/spur-core/src/orchestrator.rs — change all three call sites
let agent_canonical = normalize_agent_name(&agent_name);
let _ = ct.end_session(&session_id, /* add canonical */, status, duration, cfg.cost_tier);
// → requires a signature change to CostTracker::end_session to accept canonical agent,
//   OR pass it at start_session and let end_session use the stored value.
```

Cleaner: pass canonical at `start_session` (which already accepts `agent: &str`). The three `end_session` sites remain unchanged; only `start_session` normalizes.

Historical data migration (one-time): on `CostTracker::open`, run
```sql
UPDATE sessions SET agent = REPLACE(REPLACE(REPLACE(REPLACE(agent,
    '-acp',''), '_acp',''), '-cli',''), '_cli','');
```
wrapped behind a schema-version check so it runs only once.

**Test plan:**
- Unit: `start_session(..., "claude-code-acp", ...)` → row's `agent` column reads `claude-code`.
- Integration: after migration, `spur cost --week` combines `claude-code` and `claude-code-acp` rows into a single line.

**Effort:** ~40 LOC + 1 migration guard.

---

## Root Cause 4 — Defensive-programming details

### Fix R1 — Deterministic longest-prefix match in `PricingRegistry::get()` (resolves C3 regression)

**Root cause:** Real Claude model slug is `claude-haiku-4-5-20251001`. Registry has `claude-haiku-4`. The old substring fallback (nondeterministic) matched via the `key.contains(k)` branch. My C3 patch removed the fallback entirely. Haiku 4.5 now returns None → falls to time-tier. Net regression.

**Fix (v2, adjusted after review):** the match must require the prefix end at a dash boundary or at end-of-string. Without the boundary guard a registered `"gpt-4"` would incorrectly match `"gpt-4o"` (a different model family). Invariant: *registered canonical is a dash-delimited prefix of the slug*.

```rust
pub fn get(&self, model: &str) -> Option<&ModelPricing> {
    if model.is_empty() { return None; }
    let key = model.to_lowercase();

    // 1. exact
    if let Some(p) = self.models.get(&key) { return Some(p); }

    // 2. alias
    if let Some(canon) = self.aliases.get(&key) {
        if let Some(p) = self.models.get(canon) { return Some(p); }
    }

    // 3. deterministic dash-bounded longest-prefix match.
    //    Real slug:        "claude-haiku-4-5-20251001"
    //    Registered:       "claude-haiku-4-5", "claude-haiku-4"
    //    Result:           "claude-haiku-4-5" (longest) wins.
    //    Non-match guard:  "gpt-4" does NOT match slug "gpt-4o"
    //                      because byte after "gpt-4" is 'o', not '-'.
    let mut candidates: Vec<(&String, &ModelPricing)> = self.models.iter().collect();
    candidates.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then(a.cmp(b)));
    for (k, v) in candidates {
        let kb = k.as_bytes();
        if key.as_bytes().starts_with(kb)
            && (key.len() == kb.len()
                || key.as_bytes().get(kb.len()) == Some(&b'-'))
        {
            return Some(v);
        }
    }
    None
}
```

Properties:
- Deterministic: sort by length-desc, tie-break lexicographic.
- `get("")` → None (guarded at top).
- `get("claude-haiku-4-5-20251001")` → matches `claude-haiku-4-5` if present, else `claude-haiku-4`.
- `get("gpt-5-codex-experimental-2026")` → matches `gpt-5-codex` before `gpt-5`.
- `get("gpt-4o")` → does NOT fall through to registered `"gpt-4"` (boundary guard rejects 'o' at position 5). `gpt-4o` must be registered explicitly.
- `get("totally-unknown-model-xyz")` → None (no registered name is a dash-prefix).

**Test plan:**
```rust
#[test]
fn test_registry_dash_bounded_longest_prefix_match() {
    let reg = PricingRegistry::with_builtin_prices();
    assert!(reg.get("claude-haiku-4-5-20251001").is_some());
    assert!(reg.get("gpt-5-codex-2026-preview").is_some());
    assert!(reg.get("totally-unknown").is_none());
    assert!(reg.get("").is_none());

    // Tie-break: longest wins.
    let mut r2 = PricingRegistry::new();
    r2.insert("foo", /* A */);
    r2.insert("foo-bar", /* B */);
    assert_eq!(r2.get("foo-bar-baz"), r2.get("foo-bar"));

    // Boundary guard: "gpt-4" must NOT match "gpt-4o".
    let mut r3 = PricingRegistry::new();
    r3.insert("gpt-4", /* A */);
    assert!(r3.get("gpt-4o").is_none(),
            "gpt-4 must not swallow gpt-4o (different family)");
    assert!(r3.get("gpt-4-turbo").is_some(),
            "gpt-4 should match gpt-4-turbo (dash-bounded)");
}
```

Also register the real date-versioned canonical roots in `with_builtin_prices`:
- `claude-haiku-4-5` (new)
- `claude-opus-4-5` (new)
- `claude-sonnet-4-5` (new)

**Effort:** ~30 LOC + tests.

**Status:** ship this before C3 merges to main. Current C3 on `fix/l9-review-criticals` is a regression otherwise.

### Fix MED-2 — Shared agent-path resolver

**Root cause:** `spur-context::engine::discover_claude_dir()` treats `$CLAUDE_CONFIG_DIR` as the literal projects path; `spur-cost::ingest::claude::discover_paths()` joins `/projects`. Real upstream contract (Claude CLI) is the latter — `CLAUDE_CONFIG_DIR` is the parent.

**Fix:** new module `spur-acp::agent_paths` (or a small `spur-paths` crate):
```rust
pub fn claude_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(env) = std::env::var("CLAUDE_CONFIG_DIR") {
        for part in env.split(',') {
            let p = PathBuf::from(part.trim()).join("projects");
            if p.is_dir() { out.push(p); }
        }
        return out;
    }
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        for leaf in [".config/claude/projects", ".claude/projects"] {
            let p = home.join(leaf);
            if p.is_dir() { out.push(p); }
        }
    }
    out
}
pub fn codex_dirs() -> Vec<PathBuf> { /* CODEX_HOME/sessions, ~/.codex/sessions */ }
pub fn kiro_dirs() -> Vec<PathBuf> { /* KIRO_HOME/sessions, ~/.kiro/sessions */ }
```

Both `spur-cost::ingest::*` and `spur-context::engine::discover_*_dir` delegate to this. The engine's current per-agent `discover_*_dir` functions become 2-line wrappers.

**Test plan:**
- Unit: `CLAUDE_CONFIG_DIR=/tmp/a,/tmp/b spur_paths::claude_dirs()` returns `[/tmp/a/projects, /tmp/b/projects]` (filtered to existing).
- Parity test: both crates use the same resolver; assert equivalence via a common test fixture.

**Effort:** ~80 LOC + migration of 4 existing call sites.

### Fix LOW-2 — Resilient JSONL discovery

**Root cause:** `find_jsonl_files` uses `?` on every `read_dir` entry; one permission-denied subdir causes the whole walk to return Err; `has_jsonl_files` treats that as false.

**Fix:** depend on `walkdir = "2"`:
```rust
fn find_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    use walkdir::WalkDir;
    WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| match e {
            Ok(e) => Some(e),
            Err(err) => { tracing::debug!(error = %err, "walk error ignored"); None }
        })
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .map(|e| e.path().to_path_buf())
        .collect()
}
```
Drop the `Result<>` wrapping; discovery is best-effort.

**Test plan:**
- Unit: create a tempdir with one readable subdir containing a JSONL + one unreadable (chmod 000) subdir. Current code returns empty; fixed code returns the one readable file.

**Effort:** ~30 LOC + `walkdir` dep (~200 KB compiled).

---

## Recommended sequencing & effort

| Phase | Scope | Dependencies | Effort | User-facing? |
|---|---|---|---|---|
| **0** | Fix R1 (C3 dash-bounded longest-prefix match) + registered Haiku/Opus/Sonnet 4.5 canonical roots | none | S (30 LOC) | n/a (library) |
| **1** | Revert C1/C2; commit Fix 1A (`json_extract` + CRLF handling); commit real JSONL fixtures; integration test | 0 | M (300 LOC) | n/a (library) |
| **2** | Fix 2A (`spur cost --engine duckdb`, **--experimental flag only**); Fix 3B (quiet default log) | 1 | M (150 LOC) | behind flag |
| **2.5** | Fix 2.5 (persistent DuckDB cache + `scan_manifest` incremental refresh). **Gates user-facing ship.** | 2 | M (250 LOC) | yes — promotes 2A out of --experimental |
| **3** | Fix 3A (range breakdowns) + Fix 3C (agent normalize) | 2.5 | S (80 LOC) | yes |
| **4** | Fix 2E (retire SQLite cost reads as default) | 2.5 verified in user hands | L (crate API surface) | yes |
| **5** | Fix MED-2 (shared path resolver) + Fix LOW-2 (walkdir) | 1 | S+S (110 LOC) | n/a (library) |

Minimum viable correctness shipped to users: **Phases 0 + 1 + 2 + 2.5**. Phase 2 alone is a prototype (rescans 1.8 GB per invocation); do not promote it out of `--experimental` until 2.5 lands. Everything else is polish that can land incrementally.

---

## What changes on the `fix/l9-review-criticals` branch

1. **Replace** commit `708af7f` (C3) — keep the test rename, but restore a deterministic longest-prefix match per Fix R1. Add `claude-haiku-4-5`, `claude-opus-4-5`, `claude-sonnet-4-5` to the registry.
2. **Revert** commit `c3d4ca9` (C1) — the SQL works against fixtures but not real data.
3. **Revert** commit `8c948ff` (C2) — same.
4. **Add** a new commit implementing Fix 1A plus real-JSONL fixtures plus integration test.

Net branch diff: ~250 LOC changed from current state, +2 small fixtures.

---

## Empirical grounding artifacts

All solutions above were grounded against real data before writing:

- Fix 1A verified: in-tree probe read 492,524 rows of user's real `~/.claude/projects`, correctly extracted `type`, `sessionId`, `message.usage.input_tokens`, `message.usage.output_tokens`, `message.model`. TRY_CAST required to survive one malformed-unicode row out of 492k.
- Fix R1 verified: real Claude model slug is `claude-haiku-4-5-20251001`. The old substring fallback's `key.contains(k)` branch was the only thing keeping Haiku 4.5 sessions from falling to time-tier. My C3 commit dropped that. R1's deterministic longest-prefix is strictly better: right answer, no nondeterminism.
- Fix 3C root cause verified via grep: `normalize_agent_name` exists at `spur-core/src/orchestrator.rs:71`; three `end_session` call sites do NOT use it for the written agent string.
- Fix HIGH-1 root cause verified via grep: `end_session_with_tokens` has zero callers in the orchestrator; all three `ct.end_session` call sites use the time-tier path. Fix 1A makes this irrelevant (DuckDB reads tokens straight from JSONL).
- CRIT-2 verified by reading `spur-cli/src/main.rs:411-464`: CLI never constructs `AnalyticsEngine`. Zero users reach the analytics engine today.

If you want any of these executed as delegated tasks, I'll route codex/kimi through the usual loop. Otherwise this doc is the full design — awaiting your decision on Phase 0 first.
