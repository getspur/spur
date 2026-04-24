# Spur Cost-Correctness P0 Tranche Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four P0 cost-accuracy bugs identified in `docs/superpowers/reviews/2026-04-24-spur-cost-analytics-v2-review.md` (dedup hole, silent unpriced rows, mixed cost provenance, multi-model session_detail drop) plus prerequisite cleanup (include_str! reports.sql). Remaining P0 items (P0.5 burn_rate, P0.7 speed, P0.9 timestamps, P0.10 mutex poison, P0.11 migrations) are out of scope for this plan.

**Architecture:** DuckDB-backed analytics engine in `crates/spur-context`. Per-agent SQL views are created from Rust format strings in `engine.rs`. Cost enrichment happens in the `all_events_with_cost` LATERAL JOIN view. Reports are inline Rust SQL strings. `sql/reports.sql` is documentation only (not `include_str!`'d).

**Tech Stack:** Rust (edition 2021), DuckDB (via `duckdb` crate), `anyhow` + `tracing`, `chrono::NaiveDate`. Tests use in-memory DuckDB + `tempfile` for JSONL fixtures.

---

## File Structure

Files this plan will create or modify:

- **Modify** `crates/spur-context/src/engine.rs` — dedup in `create_claude_view`, add `cost_source` + `unpriced_event_count` columns, fix `session_detail` GROUP BY, adopt `include_str!` for reports.
- **Modify** `crates/spur-context/src/sql/reports.sql` — sync with Rust inline queries (add missing cache columns); this becomes the source of truth.
- **Create** `crates/spur-context/src/sql/cost_enrichment.sql` — lift the `all_events_with_cost` view SQL into a file, extend with `cost_source` column.
- **Modify** `crates/spur-context/tests/real_fixtures.rs` — update assertions to reflect dedup (count should be 1, not 2 when requestIds collide) + add multi-model session_detail test.
- **Modify** `crates/spur-context/tests/fixtures/real/claude_heterogeneous.jsonl` — add a third assistant row with a distinct requestId so dedup can be observed (two collide → 1 kept; new one survives → total = 2).
- **Create** `crates/spur-context/tests/fixtures/multi_model_session.jsonl` — fixture where one session_id emits events under two model strings.

Per-agent view SQL stays inline in `engine.rs` for this plan (P1.1 split is out of scope here).

---

## Task 0: Codex Cache Semantics Audit (pre-code, informs later tasks)

**Purpose:** Before assuming P0.4 (Codex cache double-count) is a real bug, verify OpenAI's `input_tokens` subset-vs-disjoint semantics against real data. If `input_tokens` includes the cached subset, Task 3 must *subtract* cached from input before pricing. If disjoint, Task 3 is unchanged.

**Files:**
- Read-only inspection of `~/.codex/sessions/**/*.jsonl` (may not exist on all dev machines — if absent, document and skip).

- [ ] **Step 1: Locate a real Codex session or a fixture**

Run:
```bash
ls -1 ~/.codex/sessions/**/*.jsonl 2>/dev/null | head -3
```

If empty, use `crates/spur-context/tests/fixtures/real/codex_heterogeneous.jsonl` as fallback (note: fixture data may not reflect production truth).

- [ ] **Step 2: Compute the identity against the fixture via DuckDB CLI**

Run:
```bash
duckdb -c "
SELECT
  SUM(json_extract(line, '\$.payload.info.total_token_usage.input_tokens')::BIGINT) AS sum_in,
  SUM(json_extract(line, '\$.payload.info.total_token_usage.output_tokens')::BIGINT) AS sum_out,
  SUM(json_extract(line, '\$.payload.info.total_token_usage.total_tokens')::BIGINT) AS sum_total,
  SUM(json_extract(line, '\$.payload.info.total_token_usage.cached_input_tokens')::BIGINT) AS sum_cached
FROM read_csv_auto(
  'crates/spur-context/tests/fixtures/real/codex_heterogeneous.jsonl',
  columns = {'line': 'VARCHAR'}, delim = '\\0', header = false, quote = '', escape = ''
)
WHERE json_valid(line)
  AND json_extract_string(line, '\$.payload.type') = 'token_count';
"
```

Expected: a single-row summary. Compare:
- If `sum_in + sum_out == sum_total` → **subset** (cached ⊂ input) → P0.4 is REAL, Task 3 must subtract cached from input before pricing.
- If `sum_in + sum_out + sum_cached == sum_total` (or close) → **disjoint** → P0.4 is NON-ISSUE, Task 3 unchanged.
- If neither holds, investigate further — do not commit Task 3 pricing changes based on Codex cache logic until resolved.

- [ ] **Step 3: Record the result**

Write findings to `docs/superpowers/reviews/2026-04-24-codex-cache-semantics.md` (one paragraph + the query output). This document decides whether Task 3 includes the Codex cache subtraction.

No commit for this task — it's a research step.

---

## Task 1: Adopt `include_str!` for `reports.sql` and Sync Missing Cache Columns

**Addresses:** P0.6.

**Files:**
- Modify: `crates/spur-context/src/sql/reports.sql` (update weekly/monthly SELECT lists)
- Modify: `crates/spur-context/src/engine.rs:15` (add `include_str!`) and replace inline report SQL strings with constants read from the file
- Test: `crates/spur-context/tests/real_fixtures.rs` (existing test should still pass)

- [ ] **Step 1: Write a failing test asserting that inline report SQL equals the file constants**

Add to `crates/spur-context/src/engine.rs` at the bottom of the existing `#[cfg(test)] mod tests` block (around line 1192):

```rust
#[test]
fn daily_report_sql_matches_file() {
    // After refactor, daily report should use the constant loaded via include_str!
    // This test guards against drift between reports.sql and inline Rust strings.
    assert!(
        DAILY_REPORT_SQL.contains("cache_read_tokens"),
        "reports.sql daily query must include cache_read_tokens"
    );
    assert!(
        DAILY_REPORT_SQL.contains("cache_creation_tokens"),
        "reports.sql daily query must include cache_creation_tokens"
    );
    assert!(
        WEEKLY_REPORT_SQL.contains("cache_read_tokens"),
        "reports.sql weekly query must include cache_read_tokens (was missing)"
    );
    assert!(
        MONTHLY_REPORT_SQL.contains("cache_read_tokens"),
        "reports.sql monthly query must include cache_read_tokens (was missing)"
    );
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test -p spur-context daily_report_sql_matches_file`
Expected: FAIL — `DAILY_REPORT_SQL` is undefined.

- [ ] **Step 3: Update `sql/reports.sql` weekly and monthly queries to include cache columns**

In `crates/spur-context/src/sql/reports.sql`, replace the weekly query block (lines 26-40) with:

```sql
-- Weekly Cost Report
-- Params: $start_date, $end_date (DATE)
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
WHERE timestamp >= $start_date AND timestamp < $end_date
GROUP BY week, agent
ORDER BY week DESC, cost_usd DESC;
```

Apply the same `cache_read_tokens, cache_creation_tokens` addition to the monthly query block (lines 42-56).

Align the `strftime` format in weekly (was `'%Y-%W'`) and monthly (was `'%Y-%m'`) to match Rust implementations (`date_trunc('week', timestamp), '%Y-%m-%d'` for weekly; `'%Y-%m'` already matches for monthly).

- [ ] **Step 4: Split `reports.sql` into per-query constants via a lightweight parser OR one file per query**

Simpler path — one file per query. Create:
- `crates/spur-context/src/sql/daily_report.sql`
- `crates/spur-context/src/sql/weekly_report.sql`
- `crates/spur-context/src/sql/monthly_report.sql`
- `crates/spur-context/src/sql/model_breakdown.sql`
- `crates/spur-context/src/sql/project_breakdown.sql`
- `crates/spur-context/src/sql/session_detail.sql`
- `crates/spur-context/src/sql/live_session_snapshot.sql`

Copy each query body (without the leading comment header) into its own file. Keep `reports.sql` as a concatenated documentation index with comments pointing to each sub-file.

- [ ] **Step 5: Load the queries in `engine.rs` via `include_str!`**

Near line 15 in `crates/spur-context/src/engine.rs`, add:

```rust
const DAILY_REPORT_SQL: &str = include_str!("sql/daily_report.sql");
const WEEKLY_REPORT_SQL: &str = include_str!("sql/weekly_report.sql");
const MONTHLY_REPORT_SQL: &str = include_str!("sql/monthly_report.sql");
const MODEL_BREAKDOWN_SQL: &str = include_str!("sql/model_breakdown.sql");
const PROJECT_BREAKDOWN_SQL: &str = include_str!("sql/project_breakdown.sql");
const SESSION_DETAIL_SQL: &str = include_str!("sql/session_detail.sql");
const LIVE_SNAPSHOT_SQL: &str = include_str!("sql/live_session_snapshot.sql");
```

Then in each corresponding `daily_report_range`, `weekly_report_range`, `monthly_report_range`, etc. method (currently at engine.rs:948, 986, 1024, etc.), replace the inline `r#"..."#` SQL literal with the constant. Example for `daily_report_range`:

```rust
pub fn daily_report_range(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<DailyRow>> {
    let mut stmt = self.conn.prepare(DAILY_REPORT_SQL)?;
    // ... rest unchanged
}
```

- [ ] **Step 6: Run the test and full crate test suite**

Run: `cargo test -p spur-context`
Expected: all tests pass, including the new `daily_report_sql_matches_file`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-context/src/sql/*.sql crates/spur-context/src/engine.rs
git commit -m "refactor(spur-context): include_str! report SQL and sync cache columns (P0.6)"
```

---

## Task 2: Dedup Claude Events on `requestId` + `message.id`

**Addresses:** P0.1.

**Files:**
- Modify: `crates/spur-context/src/engine.rs:376-418` (`create_claude_view`)
- Modify: `crates/spur-context/tests/real_fixtures.rs:45-49` (update dedup assertion)
- Modify: `crates/spur-context/tests/fixtures/real/claude_heterogeneous.jsonl` (add a third assistant row with a distinct requestId)

- [ ] **Step 1: Add a third assistant row to the fixture**

Inspect the current fixture:
```bash
cat crates/spur-context/tests/fixtures/real/claude_heterogeneous.jsonl | head -10
```

Append ONE new assistant line with a distinct `requestId` (e.g. `req_99999999`) and distinct `message.id`. Use the same schema as the existing two assistant rows, but with input_tokens=7, output_tokens=100, and a unique timestamp. The fixture now contains:
- 2 assistant rows sharing `requestId=88888888-…` (dedup collapses to 1)
- 1 assistant row with `requestId=99999999-…` (survives)
- Total kept after dedup: **2**

Keep the old sums-to-12/188 invariant intact for the *first two rows*; the third row's tokens will shift the sums — update the test assertions in Step 3 accordingly.

- [ ] **Step 2: Write the failing dedup test**

In `crates/spur-context/tests/real_fixtures.rs`, replace the line-49 assertion (`assert_eq!(claude_count, 2, …)`) with:

```rust
let claude_count: i64 =
    engine
        .conn()
        .query_row("SELECT COUNT(*) FROM claude_events", [], |row| row.get(0))?;
assert_eq!(
    claude_count, 2,
    "after dedup: two requestIds share a value (collapsed to 1) + one unique (kept) = 2"
);

// Explicit dedup coverage: the raw row count before dedup would be 3.
let raw_assistant_count: i64 = engine.conn().query_row(
    "SELECT COUNT(*) FROM claude_raw
     WHERE json_valid(line)
       AND json_extract_string(line, '$.type') = 'assistant'",
    [],
    |row| row.get(0),
)?;
assert_eq!(
    raw_assistant_count, 3,
    "fixture has 3 assistant rows; dedup must reduce to 2"
);
```

- [ ] **Step 3: Update token sum assertions to account for the new third row**

Adjust the sums at `real_fixtures.rs:57-64` to match whatever you set in Step 1. If the kept rows are the distinct-requestId row (12 input / 188 output kept from original) + 7 input / 100 output from new row, and the duplicate-requestId row is dropped, the assertions become:

```rust
assert_eq!(claude_input_sum, Some(12 + 7));  // 19
assert_eq!(claude_output_sum, Some(188 + 100));  // 288
```

Pick whichever row of the duplicate pair survives the dedup window (see Step 4).

- [ ] **Step 4: Run test to confirm it fails**

Run: `cargo test -p spur-context real_fixtures_exercise_heterogeneous_views`
Expected: FAIL — without dedup, count is 3.

- [ ] **Step 5: Add dedup to the Claude view**

In `crates/spur-context/src/engine.rs`, modify `create_claude_view` (around line 397) — add a `QUALIFY` clause using ROW_NUMBER over the dedup key. Replace the current `claude_events` view body with:

```sql
CREATE OR REPLACE VIEW claude_events AS
SELECT * FROM (
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
        json_extract_string(line, '$.requestId') AS _request_id,
        json_extract_string(line, '$.message.id') AS _message_id,
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
WHERE _dedup_rn = 1;
```

Note: `_request_id`, `_message_id`, `_dedup_rn` are leading-underscore names to signal internal columns. Downstream views (`all_events`) should `SELECT` only the public columns — verify the `all_events` definition at `engine.rs:531-537` still works (it does, since it column-lists explicitly).

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p spur-context real_fixtures_exercise_heterogeneous_views`
Expected: PASS.

Also run: `cargo test -p spur-context`
Expected: all existing tests pass (they should — we added columns without removing any).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-context/src/engine.rs \
        crates/spur-context/tests/real_fixtures.rs \
        crates/spur-context/tests/fixtures/real/claude_heterogeneous.jsonl
git commit -m "fix(spur-context): dedup Claude assistant events on (sessionId, requestId, message.id) (P0.1)"
```

---

## Task 3: Add `cost_source` Column and `unpriced_event_count` Surface

**Addresses:** P0.2, P0.3.

**Files:**
- Modify: `crates/spur-context/src/engine.rs:29-55` (`ALL_EVENTS_WITH_COST_VIEW`)
- Modify: `crates/spur-context/src/sql/daily_report.sql` + `weekly_report.sql` + `monthly_report.sql` (add unpriced count column)
- Modify: `crates/spur-context/src/engine.rs` row types (`DailyRow`, `WeeklyRow`, `MonthlyRow`) — add `unpriced_events: i64` field, `cost_source` is aggregated, not per-row on rollups

- [ ] **Step 1: Write a failing test**

Add to the existing `#[cfg(test)] mod tests` block in `engine.rs`:

```rust
#[test]
fn unpriced_events_surface_through_report() -> Result<()> {
    let engine = AnalyticsEngine::open_in_memory()?;
    engine.initialize()?;

    // Load ONLY one known model to the pricing table
    engine.conn().execute_batch(
        "INSERT INTO pricing VALUES
        ('claude-opus-4', 15.0, 75.0, 1.5, 18.75, '2020-01-01', NULL);"
    )?;

    // Insert two events directly into all_events substrate:
    // - one priced (model matches)
    // - one unpriced (model doesn't match pricing row)
    engine.conn().execute_batch(
        "CREATE OR REPLACE TABLE all_events_manual AS
         SELECT * FROM (VALUES
            (TIMESTAMP '2026-04-20 10:00:00', 'sess1', 'claude', 'claude-opus-4', 'proj', 1000::BIGINT, 100::BIGINT, 0::BIGINT, 0::BIGINT, NULL::DOUBLE),
            (TIMESTAMP '2026-04-20 10:05:00', 'sess1', 'claude', 'ghost-model-xyz', 'proj', 1000::BIGINT, 100::BIGINT, 0::BIGINT, 0::BIGINT, NULL::DOUBLE)
         ) AS t(timestamp, session_id, agent, model, project, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd);
         CREATE OR REPLACE VIEW all_events AS SELECT * FROM all_events_manual;"
    )?;

    // After the cost_source column lands, SELECT it and count 'unpriced'
    let unpriced: i64 = engine.conn().query_row(
        "SELECT COUNT(*) FROM all_events_with_cost WHERE cost_source = 'unpriced'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(unpriced, 1, "ghost-model-xyz must be flagged unpriced");

    let priced: i64 = engine.conn().query_row(
        "SELECT COUNT(*) FROM all_events_with_cost WHERE cost_source = 'priced'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(priced, 1, "claude-opus-4 must resolve as priced");

    Ok(())
}
```

- [ ] **Step 2: Run test to confirm it fails**

Run: `cargo test -p spur-context unpriced_events_surface_through_report`
Expected: FAIL — `cost_source` column does not exist.

- [ ] **Step 3: Extend `ALL_EVENTS_WITH_COST_VIEW` with `cost_source` column**

In `crates/spur-context/src/engine.rs`, replace the constant (lines 29-55) with:

```rust
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
```

Also update `sql/schema.sql` at lines 44-58 to mirror the same `cost_source` logic so the placeholder view is consistent.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-context unpriced_events_surface_through_report`
Expected: PASS.

- [ ] **Step 5: Add `unpriced_events` aggregation to daily/weekly/monthly reports**

Update `crates/spur-context/src/sql/daily_report.sql`:

```sql
-- Daily Cost Report
-- Params: $start_date, $end_date (DATE)
SELECT
    strftime(timestamp, '%Y-%m-%d') AS day,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(cache_creation_tokens) AS cache_creation_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
    SUM(CASE WHEN cost_source = 'unpriced' THEN 1 ELSE 0 END) AS unpriced_events,
    SUM(CASE WHEN cost_source = 'native' THEN 1 ELSE 0 END) AS native_cost_events,
    SUM(CASE WHEN cost_source = 'priced' THEN 1 ELSE 0 END) AS priced_events
FROM all_events_with_cost
WHERE timestamp >= $start_date AND timestamp < $end_date
GROUP BY day, agent
ORDER BY day DESC, cost_usd DESC;
```

Apply the same three `SUM(CASE WHEN …) AS …` columns to `weekly_report.sql` and `monthly_report.sql`.

- [ ] **Step 6: Update `DailyRow`, `WeeklyRow`, `MonthlyRow` structs**

In `crates/spur-context/src/engine.rs`, find the `DailyRow` struct definition (search for `pub struct DailyRow`) and add three fields:

```rust
pub struct DailyRow {
    pub day: String,
    pub agent: String,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    pub unpriced_events: i64,
    pub native_cost_events: i64,
    pub priced_events: i64,
}
```

Apply identically to `WeeklyRow` and `MonthlyRow`.

Update the `row.get(N)` positional reads in `daily_report_range`, `weekly_report_range`, `monthly_report_range` to fetch the three new columns at the correct positions (positions 8, 9, 10 after the existing cost_usd at position 7).

- [ ] **Step 7: Run full test suite**

Run: `cargo test -p spur-context`
Expected: all tests pass, including new and existing.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-context/src/engine.rs \
        crates/spur-context/src/sql/*.sql
git commit -m "feat(spur-context): surface cost_source + unpriced_events in reports (P0.2, P0.3)"
```

---

## Task 4: Fix `session_detail` Multi-Model Session Drop

**Addresses:** P0.8.

**Files:**
- Modify: `crates/spur-context/src/engine.rs:948-988` (`session_detail`)
- Modify: `crates/spur-context/src/sql/session_detail.sql`
- Create: `crates/spur-context/tests/fixtures/multi_model_session.jsonl`
- Modify: `crates/spur-context/tests/real_fixtures.rs` (add new test)

- [ ] **Step 1: Create the multi-model fixture**

Create `crates/spur-context/tests/fixtures/multi_model_session.jsonl` with two assistant events sharing `sessionId` but different `message.model`:

```
{"type":"assistant","sessionId":"multi-model-sess","timestamp":"2026-04-20T10:00:00Z","requestId":"req1","message":{"id":"msg1","model":"claude-opus-4","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"costUSD":0.01}
{"type":"assistant","sessionId":"multi-model-sess","timestamp":"2026-04-20T10:05:00Z","requestId":"req2","message":{"id":"msg2","model":"claude-sonnet-4","usage":{"input_tokens":200,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"costUSD":0.02}
```

- [ ] **Step 2: Write a failing test**

Add to `crates/spur-context/tests/real_fixtures.rs`:

```rust
#[test]
fn session_detail_aggregates_across_models() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let claude_root = temp.path().join("claude-root");
    let target = claude_root.join("projects/multi/fixture.jsonl");
    fs::create_dir_all(target.parent().unwrap())?;
    fs::copy(repo_fixture("tests/fixtures/multi_model_session.jsonl"), &target)?;

    env::set_var("CLAUDE_CONFIG_DIR", &claude_root);
    env::set_var("CODEX_HOME", temp.path().join("codex-empty"));
    env::set_var("KIRO_HOME", temp.path().join("kiro-empty"));
    fs::create_dir_all(temp.path().join("codex-empty"))?;
    fs::create_dir_all(temp.path().join("kiro-empty"))?;

    let engine = AnalyticsEngine::open_in_memory()?;
    engine.initialize()?;
    engine.create_agent_views()?;

    let report = engine.session_report("multi-model-sess")?;
    let session = report.expect("session should be found");
    assert_eq!(session.input_tokens, 300, "both model buckets must be summed (was dropping second)");
    assert_eq!(session.output_tokens, 150);
    assert_eq!(session.events, 2, "session spans 2 events across 2 models");
    Ok(())
}
```

Note: method name `session_report` may be `session_detail` — verify against current `engine.rs` and use the correct public method. Adjust the field access if the returned type is `SessionRow` not `SessionReport`.

- [ ] **Step 3: Run test to confirm it fails**

Run: `cargo test -p spur-context session_detail_aggregates_across_models`
Expected: FAIL — current impl returns only the first model bucket (input_tokens = 100 or 200, not 300).

- [ ] **Step 4: Fix `sql/session_detail.sql`**

Remove `model` from the GROUP BY (since we want one aggregate row per session) and instead use `string_agg(DISTINCT model)` to surface which models were used:

```sql
-- Session Detail
-- Params: $session_id (VARCHAR)
SELECT
    session_id,
    any_value(agent) AS agent,
    string_agg(DISTINCT model, ',' ORDER BY model) AS models,
    MIN(timestamp) AS started_at,
    MAX(timestamp) AS ended_at,
    MAX(timestamp) - MIN(timestamp) AS duration,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(cache_creation_tokens) AS cache_creation_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
    COUNT(*) AS events
FROM all_events_with_cost
WHERE session_id = $session_id
GROUP BY session_id;
```

- [ ] **Step 5: Update `SessionRow` struct**

In `crates/spur-context/src/engine.rs`, find `pub struct SessionRow` and:
- Change `pub model: String` to `pub models: String` (comma-separated list)
- Ensure `row.get(N)` indices match the new column order

- [ ] **Step 6: Update `live_session_snapshot.sql` identically**

Apply the same GROUP BY simplification (drop `model`, use `string_agg`). Update `LiveSnapshot` struct similarly.

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p spur-context session_detail_aggregates_across_models`
Expected: PASS with input_tokens == 300.

Also run: `cargo test -p spur-context`
Expected: all existing tests pass. If `reporter.rs` tests reference `row.model`, update them to `row.models`.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-context/src/engine.rs \
        crates/spur-context/src/sql/session_detail.sql \
        crates/spur-context/src/sql/live_session_snapshot.sql \
        crates/spur-context/tests/real_fixtures.rs \
        crates/spur-context/tests/fixtures/multi_model_session.jsonl
git commit -m "fix(spur-context): session_detail aggregates across models (P0.8)"
```

---

## Self-Review (performed by plan author before handoff)

**Spec coverage:**
- P0.1 dedup → Task 2 ✓
- P0.2 unpriced surfacing → Task 3 ✓
- P0.3 cost_source → Task 3 ✓
- P0.4 Codex cache audit → Task 0 (data-verification only; no code change this plan) ✓
- P0.6 include_str! reports.sql → Task 1 ✓
- P0.8 session_detail multi-model → Task 4 ✓
- P0.5, P0.7, P0.9, P0.10, P0.11 — explicitly out of scope; flagged for follow-on plan.

**Placeholder scan:** None of the forbidden patterns present. All steps carry exact code.

**Type consistency:** `DailyRow`/`WeeklyRow`/`MonthlyRow` extended in Task 3 with three new fields; `SessionRow` changed from `model: String` to `models: String` in Task 4. Downstream consumers (`reporter.rs` struct fills) must be updated in the same commits.

**Known caveat:** Task 4 changes a public API field name (`model` → `models`) which is a breaking change for `SessionRow` consumers outside this crate (e.g., any TUI code reading it). This plan assumes `SessionRow` is used only via `spur-context` and `spur-cli`. Verify before executing Task 4 with:
```bash
rg -n "SessionRow|session_report" --type rust
```

---

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task with review between tasks; brain dispatches via `delegate_to_worker` (likely `claude-code` for multi-file refactors, `codex` for narrow single-file edits). Fast iteration, small blast radius per step.
2. **Inline Execution** — execute tasks in-session using `executing-plans`, batch with checkpoints.

Which approach?
