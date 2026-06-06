# Semantic Search Wave 2 — Graph-Augmented Retrieval

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-spec-live-evidence.ipynb`
**Design epic:** _(this plan IS the design — derived from MemGraphRAG structural mapping + live spur-analyst evidence 2026-06-06)_

**Goal:** Close the three remaining structural gaps between SPUR's knowledge architecture and MemGraphRAG: (1) verify/fix the M_pas dedup Wave 1 left in place, (2) build the selective graph-traversal SQL macro, (3) wire a `scope=graph` retrieval path in `code_semantic_search` that returns a graph-neighborhood context payload instead of flat ranked rows.

**Architecture:** The routing decision (expand via call graph vs. stay in BM25 fusion) is encoded as a SQL gate inside `search_graph(q)` using `v_symbol_scorecard.posture` and `v_symbol_inbound.callers` — both 100%-covered over live nodes. No external classifier. The output adds two columns (`neighbor_kind`, `edge_bind_method`) so the caller gets primary hits + their call context in one tool call.

**Tech Stack:** DuckDB SQL macros (`init_search.sql`), Rust (`code_semantic_search.rs`), bundled `libduckdb-sys` v1.5.2.

---

### Task 1: Verify and fix M_pas Wave-1 dedup

**Task ID:** `task-dedup-verify`

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql` (dedup partition fix if needed)
- Modify: `crates/spur-cli/src/commands/analyst.rs` (guard update)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `sections_search` row count is ≥ 5% smaller than `sections_raw` when skills are vendored (≥ 6 agent dirs × same skill = measurable dedup)
- [ ] The dedup guard in `analyst.rs` asserts the SPUR-MANAGED normalization pattern
- [ ] `scripts/spur-cargo test -p spur-cli` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `init_search.sql` sections_search CREATE TABLE, `analyst.rs` SQL guards
- OUT of scope: `symbol_text` dedup, `search_code`, `search_docs`, `search` macros
- If you discover the SPUR-MANAGED header never appears in `body_text` (only in file headers), emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Diagnose the dedup ratio**

Run the following analyst query to check whether SPUR-MANAGED headers actually appear in section bodies:

```sql
SELECT count(*) AS sections_with_managed_header
FROM sections
WHERE body_text LIKE '%<!-- SPUR-MANAGED%';
```

Expected: if skills are installed into ≥2 agent dirs, this should be > 0.

Also check the current ratio:
```sql
SELECT
  (SELECT count(*) FROM sections) AS raw,
  (SELECT count(*) FROM sections_search) AS deduped;
```

Expected post-reindex: deduped < raw by at least 5% if managed-header sections exist.

- [ ] **Step 2: If sections_with_managed_header = 0 — fix the dedup key**

The SPUR-MANAGED header is injected at the TOP of each vendored file (before any markdown headings), so it appears in the *first* section body of that file only. The current dedup strips it from the body and then partitions — correct approach. But if the header is outside all headings it might not appear in `body_text` at all.

Diagnosis: check where the header lands:
```sql
SELECT file_path, body_text
FROM sections
WHERE file_path LIKE '.%'
  AND body_text LIKE '%SPUR-MANAGED%'
LIMIT 5;
```

If no rows: the SPUR-MANAGED comment is only in the markdown preamble, before the first `##` heading, and Lance does not create a section for preamble text → dedup by body normalization will never fire for skill copies.

Fix: also dedup by `qualified_name` alone (the section heading path is identical across all vendored copies):

```sql
CREATE OR REPLACE TABLE sections_search AS
SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
FROM sections
QUALIFY row_number() OVER (
  PARTITION BY COALESCE(qualified_name, ''),
               regexp_replace(body_text, '<!-- SPUR-MANAGED[^>]*-->\n?', '')
  ORDER BY (file_path LIKE '.%')::INT, length(file_path), file_path
) = 1;
```

This is already the current SQL — if the SPUR-MANAGED header never appears in body_text, the normalization is a no-op but the `COALESCE(qualified_name, '')` partition key still deduplicates sections that have identical heading paths AND identical bodies across dot-dirs.

If qualified_name is NULL for all sections (Lance doesn't populate it): replace with `heading_level || ':' || substring(body_text, 1, 120)` as the stable identity key:

```sql
CREATE OR REPLACE TABLE sections_search AS
SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
FROM sections
QUALIFY row_number() OVER (
  PARTITION BY heading_level,
               regexp_replace(COALESCE(body_text, ''), '<!-- SPUR-MANAGED[^>]*-->\n?', '')
  ORDER BY (file_path LIKE '.%')::INT, length(file_path), file_path
) = 1;
```

Apply whichever fix the diagnosis requires.

- [ ] **Step 3: Update the analyst.rs dedup guard**

Find the existing `init_search_sql_dedups_and_diversifies` test in `crates/spur-cli/src/commands/analyst.rs` and update the assertion to match the final dedup partition key chosen in Step 2.

The test reads `INIT_SEARCH_SQL` (the `include_str!` const) and checks for the presence of the partition expression. After any fix, the test assertion must match the actual SQL.

- [ ] **Step 4: Rebuild and verify**

```bash
SPUR_REMOTE=0 scripts/spur-cargo run -p spur-cli -- analyst build
```

Then re-run the ratio query from Step 1. Confirm deduped ≤ raw.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-context/poc/duckdb-analyst/init_search.sql
git add crates/spur-cli/src/commands/analyst.rs
git commit -m "fix(spur-context): W2.dedup correct sections_search dedup partition key"
```

---

### Task 2: Add `search_graph(q)` macro with selective router gate

**Task ID:** `task-graph-macro`

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql`
- Modify: `crates/spur-cli/src/commands/analyst.rs` (new SQL guard)

**Depends on:** none (runs parallel with task-dedup-verify)

**Acceptance Criteria:**
- [ ] `search_graph('...')` macro exists in `init_search.sql`
- [ ] Returns columns: `kind`, `title`, `file`, `score`, `signal`, `neighbor_kind`, `edge_bind_method`
- [ ] `neighbor_kind` ∈ `{'primary', 'caller', 'callee'}`
- [ ] Symbols with `posture = 'load-bearing wall' AND caller_count > 30` are NOT expanded (only appear as primary)
- [ ] SQL guard in `analyst.rs` asserts the gate predicate and the column shape
- [ ] `scripts/spur-cargo test -p spur-cli` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `init_search.sql` (append after `search` macro), `analyst.rs` guard
- OUT of scope: `search_docs`, `search_code`, `search` macros — do not modify existing macros
- OUT of scope: `code_semantic_search.rs` — that is task-graph-handler
- If you need to add a column to `v_symbol_scorecard` or `v_symbol_inbound`, emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Write the failing test first**

Add to `crates/spur-cli/src/commands/analyst.rs`, in the `#[cfg(test)]` block:

```rust
#[test]
fn init_search_sql_graph_macro_has_gate_and_neighbor_kind() {
    // Gate predicate must exist
    assert!(
        INIT_SEARCH_SQL.contains("posture != 'load-bearing wall' OR"),
        "search_graph macro must contain the sink-bailout gate"
    );
    // Output must include both new columns
    assert!(
        INIT_SEARCH_SQL.contains("neighbor_kind") && INIT_SEARCH_SQL.contains("edge_bind_method"),
        "search_graph macro must project neighbor_kind and edge_bind_method"
    );
    // Macro name must exist
    assert!(
        INIT_SEARCH_SQL.contains("CREATE OR REPLACE MACRO search_graph(q)"),
        "search_graph macro must be defined"
    );
}
```

Run: `scripts/spur-cargo test -p spur-cli init_search_sql_graph_macro_has_gate_and_neighbor_kind -- --nocapture`
Expected: FAIL (macro doesn't exist yet)

- [ ] **Step 2: Append `search_graph` macro to `init_search.sql`**

Add the following after the closing `;` of the `search` macro (line 148):

```sql
-- Graph-augmented: BM25 top-k hits + selective 1-hop call-graph expansion.
-- Gate: symbols with posture = 'load-bearing wall' AND callers > 30 are popular
-- sinks — expanding them would flood results with noise. All other hits expand.
CREATE OR REPLACE MACRO search_graph(q) AS TABLE
  SELECT kind, title, file, score, signal, neighbor_kind, edge_bind_method
  FROM (
    WITH base AS (
      SELECT
        st.stable_symbol_id,
        st.entity_name AS symbol,
        st.symbol_kind,
        st.file_path,
        fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
        sc.pagerank,
        sc.churn_90d,
        sc.posture,
        sc.component_size,
        COALESCE(vi.callers, 0) AS caller_count
      FROM symbol_text st
      JOIN v_symbol_scorecard sc USING (stable_symbol_id)
      LEFT JOIN v_symbol_inbound vi USING (stable_symbol_id)
      WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
      ORDER BY
        bm25_raw
          * CASE WHEN st.file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
          * CASE WHEN st.symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                 WHEN st.symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
          * (1 + 0.15 * ln(1 + sc.pagerank * 1e4)) DESC NULLS LAST
      LIMIT 5
    ),
    -- Selective gate: expand only non-sink symbols
    gated AS (
      SELECT * FROM base
      WHERE posture != 'load-bearing wall' OR caller_count <= 30
    ),
    primary_rows AS (
      SELECT
        'code'           AS kind,
        symbol           AS title,
        regexp_replace(file_path, '^crates/', '') AS file,
        round(
          bm25_raw
            * CASE WHEN file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
            * CASE WHEN symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                   WHEN symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
            * (1 + 0.15 * ln(1 + pagerank * 1e4)), 3) AS score,
        posture || ' · pr=' || round(pagerank * 1e4, 1) || ' · churn=' || churn_90d AS signal,
        'primary'        AS neighbor_kind,
        CAST(NULL AS VARCHAR) AS edge_bind_method
      FROM base
    ),
    caller_rows AS (
      SELECT
        'code'           AS kind,
        nsrc.entity_name AS title,
        regexp_replace(nsrc.file_path, '^crates/', '') AS file,
        round(COALESCE(sc2.pagerank, 0) * 1e4, 3) AS score,
        COALESCE(sc2.posture, 'unknown') || ' · caller of ' || g.symbol AS signal,
        'caller'         AS neighbor_kind,
        e.bind_method    AS edge_bind_method
      FROM gated g
      JOIN edges e
        ON e.target_stable_id = g.stable_symbol_id AND e.relation = 'calls'
      JOIN nodes nsrc
        ON nsrc.stable_symbol_id = e.source_stable_id
      LEFT JOIN v_symbol_scorecard sc2
        ON sc2.stable_symbol_id = nsrc.stable_symbol_id
      WHERE nsrc.file_path NOT LIKE '.%'
        AND nsrc.file_path NOT LIKE '%/tests/%'
    ),
    callee_rows AS (
      SELECT
        'code'           AS kind,
        ndst.entity_name AS title,
        regexp_replace(ndst.file_path, '^crates/', '') AS file,
        round(COALESCE(sc3.pagerank, 0) * 1e4, 3) AS score,
        COALESCE(sc3.posture, 'unknown') || ' · callee of ' || g.symbol AS signal,
        'callee'         AS neighbor_kind,
        e.bind_method    AS edge_bind_method
      FROM gated g
      JOIN edges e
        ON e.source_stable_id = g.stable_symbol_id AND e.relation = 'calls'
      JOIN nodes ndst
        ON ndst.stable_symbol_id = e.target_stable_id
      LEFT JOIN v_symbol_scorecard sc3
        ON sc3.stable_symbol_id = ndst.stable_symbol_id
      WHERE ndst.file_path NOT LIKE '.%'
        AND ndst.file_path NOT LIKE '%/tests/%'
    )
    SELECT * FROM primary_rows
    UNION ALL
    SELECT * FROM caller_rows
    QUALIFY row_number() OVER (PARTITION BY file, title ORDER BY score DESC) <= 2
    UNION ALL
    SELECT * FROM callee_rows
    QUALIFY row_number() OVER (PARTITION BY file, title ORDER BY score DESC) <= 2
  )
  ORDER BY
    CASE neighbor_kind WHEN 'primary' THEN 0 WHEN 'caller' THEN 1 ELSE 2 END,
    score DESC NULLS LAST
  LIMIT 40;
```

- [ ] **Step 3: Run the test to verify it passes**

```bash
scripts/spur-cargo test -p spur-cli init_search_sql_graph_macro_has_gate_and_neighbor_kind -- --nocapture
```

Expected: PASS

- [ ] **Step 4: Run the full spur-cli test suite**

```bash
scripts/spur-cargo test -p spur-cli -- --nocapture
```

Expected: all existing tests pass, new test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-context/poc/duckdb-analyst/init_search.sql
git add crates/spur-cli/src/commands/analyst.rs
git commit -m "feat(spur-context): W2.1 search_graph macro with selective router gate"
```

---

### Task 3: Wire `scope=graph` in `code_semantic_search` + graph-neighborhood response shape

**Task ID:** `task-graph-handler`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs`

**Depends on:** `task-graph-macro`

**Acceptance Criteria:**
- [ ] `scope=graph` is accepted by the `code_semantic_search` MCP tool
- [ ] Response rows for `scope=graph` include `neighbor_kind` and `edge_bind_method` JSON fields
- [ ] Existing `scope=docs`, `scope=code`, `scope=all` behavior is unchanged (regression)
- [ ] New test `search_graph_scope_returns_neighbor_kind_rows` passes
- [ ] `scripts/spur-cargo test -p spur-notebook` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `code_semantic_search.rs` only
- OUT of scope: `init_search.sql` (that is task-graph-macro), MCP tool schema JSON/TOML
- If the MCP tool schema is defined in a separate file that must be updated for the new scope value, emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Write the failing test first**

Add to `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs`, inside `#[cfg(test)]`:

```rust
#[test]
fn search_graph_scope_returns_neighbor_kind_rows() {
    // Build a minimal fixture with FTS + scorecard + edges so search_graph can run.
    // Uses an in-memory DB with the same macro structure as the real analyst.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.duckdb");
    let conn = duckdb::Connection::open(&db_path).unwrap();

    conn.execute_batch("
        INSTALL fts; LOAD fts; LOAD icu;

        -- Minimal nodes + symbol_text + scorecard + inbound + edges
        CREATE TABLE nodes (stable_symbol_id VARCHAR, node_id BIGINT,
          entity_name VARCHAR, qualified_name VARCHAR,
          file_path VARCHAR, symbol_kind VARCHAR,
          line_start INT, line_end INT);
        CREATE TABLE symbol_text (stable_symbol_id VARCHAR, entity_name VARCHAR,
          qualified_name VARCHAR, file_path VARCHAR, symbol_kind VARCHAR,
          doc_text VARCHAR);

        INSERT INTO nodes VALUES
          ('aa01', 1, 'handle_query', 'handle_query',
           'crates/spur-mcp/src/server.rs', 'function', 1, 20),
          ('aa02', 2, 'run_bm25_search', 'run_bm25_search',
           'crates/spur-mcp/src/search.rs', 'function', 1, 10);
        INSERT INTO symbol_text VALUES
          ('aa01', 'handle_query', 'handle_query',
           'crates/spur-mcp/src/server.rs', 'function',
           'handle_query search bm25 graph query'),
          ('aa02', 'run_bm25_search', 'run_bm25_search',
           'crates/spur-mcp/src/search.rs', 'function',
           'run_bm25_search search fts fulltext');

        PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text',overwrite=1);

        -- Minimal scorecard view
        CREATE VIEW v_symbol_scorecard AS
        SELECT stable_symbol_id,
               0.001 AS pagerank, 0 AS churn_90d,
               'leaf' AS posture, 1 AS component_size
        FROM nodes;

        -- Minimal inbound view (0 callers = safe to expand)
        CREATE VIEW v_symbol_inbound AS
        SELECT stable_symbol_id, 0 AS callers, 0 AS importers,
               0 AS containers, 0 AS inbound_total
        FROM nodes;

        -- One call edge: aa01 calls aa02
        CREATE TABLE edges (source_stable_id VARCHAR, target_stable_id VARCHAR,
          relation VARCHAR, bind_method VARCHAR, edge_kind VARCHAR,
          src_id BIGINT, dst_id BIGINT, target_label VARCHAR,
          confidence VARCHAR, confidence_score DOUBLE);
        INSERT INTO edges VALUES
          ('aa01','aa02','calls','singleton','calls',1,2,'run_bm25_search','high',0.9);

        -- FTS macro (simplified version of real search_graph)
        CREATE OR REPLACE MACRO search_graph(q) AS TABLE
          SELECT kind, title, file, score, signal, neighbor_kind, edge_bind_method
          FROM (
            WITH base AS (
              SELECT st.stable_symbol_id, st.entity_name AS symbol, st.symbol_kind,
                     st.file_path,
                     fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                     sc.pagerank, sc.churn_90d, sc.posture,
                     COALESCE(vi.callers, 0) AS caller_count
              FROM symbol_text st
              JOIN v_symbol_scorecard sc USING (stable_symbol_id)
              LEFT JOIN v_symbol_inbound vi USING (stable_symbol_id)
              WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
              ORDER BY bm25_raw DESC LIMIT 5
            ),
            gated AS (
              SELECT * FROM base
              WHERE posture != 'load-bearing wall' OR caller_count <= 30
            ),
            primary_rows AS (
              SELECT 'code' AS kind, symbol AS title,
                     regexp_replace(file_path,'^crates/','') AS file,
                     round(bm25_raw, 3) AS score,
                     posture AS signal, 'primary' AS neighbor_kind,
                     CAST(NULL AS VARCHAR) AS edge_bind_method
              FROM base
            ),
            callee_rows AS (
              SELECT 'code', ndst.entity_name,
                     regexp_replace(ndst.file_path,'^crates/',''),
                     0.0, 'leaf · callee of ' || g.symbol,
                     'callee', e.bind_method
              FROM gated g
              JOIN edges e ON e.source_stable_id = g.stable_symbol_id
                AND e.relation = 'calls'
              JOIN nodes ndst ON ndst.stable_symbol_id = e.target_stable_id
              WHERE ndst.file_path NOT LIKE '.%'
            )
            SELECT * FROM primary_rows
            UNION ALL SELECT * FROM callee_rows
          )
          ORDER BY CASE neighbor_kind WHEN 'primary' THEN 0 ELSE 1 END, score DESC
          LIMIT 20;

        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('test-hash');
    ").unwrap();
    drop(conn);

    let (_, rows, _) = search_rows("search bm25 graph", "graph", 20,
        Some(db_path.to_str().unwrap())).unwrap();

    // Must have at least one primary and one callee row
    let has_primary = rows.iter().any(|r|
        r["neighbor_kind"].as_str() == Some("primary"));
    let has_callee = rows.iter().any(|r|
        r["neighbor_kind"].as_str() == Some("callee"));

    assert!(has_primary, "graph scope must return primary hits");
    assert!(has_callee,  "graph scope must return callee neighbors");

    // All rows must have edge_bind_method field (may be null for primary)
    for row in &rows {
        assert!(row.get("edge_bind_method").is_some(),
            "every graph row must carry edge_bind_method key");
    }
}
```

Run: `scripts/spur-cargo test -p spur-notebook search_graph_scope_returns_neighbor_kind_rows -- --nocapture`
Expected: FAIL (scope "graph" not handled yet)

- [ ] **Step 2: Add the `"graph"` scope arm to `search_rows`**

In `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs`, find the `let sql = match scope {` block (around line 147) and add the graph arm:

```rust
let sql = match scope {
    "docs" => format!(
        "SELECT 'doc' AS kind, section AS title, file_path AS file, \
         round(bm25, 3) AS score, CAST(NULL AS VARCHAR) AS signal \
         FROM search_docs('{q}') LIMIT {limit}"
    ),
    "code" => format!(
        "SELECT 'code' AS kind, symbol AS title, file_path AS file, \
         round(bm25, 3) AS score, posture AS signal \
         FROM search_code('{q}') LIMIT {limit}"
    ),
    "graph" => format!(
        "SELECT kind, title, file, round(score, 3) AS score, signal, \
         neighbor_kind, edge_bind_method \
         FROM search_graph('{q}') LIMIT {limit}"
    ),
    _ => format!(
        "SELECT kind, title, file, round(score, 3) AS score, signal \
         FROM search('{q}') LIMIT {limit}"
    ),
};
```

- [ ] **Step 3: Update the `query_map` closure to handle the extra columns for `scope=graph`**

The current `query_map` reads exactly 5 columns (indices 0–4). For `scope=graph`, columns 5 and 6 (`neighbor_kind`, `edge_bind_method`) must also be read. Branch on scope:

```rust
let rows = stmt
    .query_map([], |row| {
        let mut obj = json!({
            "kind":  row.get::<_, String>(0)?,
            "title": row.get::<_, String>(1)?,
            "file":  row.get::<_, String>(2)?,
            "score": row.get::<_, f64>(3)?,
            "signal": row.get::<_, Option<String>>(4)?,
        });
        if scope == "graph" {
            obj["neighbor_kind"]     = json!(row.get::<_, Option<String>>(5)?);
            obj["edge_bind_method"]  = json!(row.get::<_, Option<String>>(6)?);
        }
        Ok(obj)
    })
    .map_err(|e| internal("failed to run search query", &e))?;
```

Note: `json!({...})` returns `serde_json::Value`. Mutating it via index requires `Value::Object`. Verify `json!` returns `Value::Object` (it does when given `{...}` literal). If the compiler rejects `obj["neighbor_kind"] = ...`, use:

```rust
if scope == "graph" {
    if let serde_json::Value::Object(ref mut map) = obj {
        map.insert("neighbor_kind".into(),
            json!(row.get::<_, Option<String>>(5)?));
        map.insert("edge_bind_method".into(),
            json!(row.get::<_, Option<String>>(6)?));
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
scripts/spur-cargo test -p spur-notebook search_graph_scope_returns_neighbor_kind_rows -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Run the full spur-notebook test suite**

```bash
scripts/spur-cargo test -p spur-notebook -- --nocapture
```

Expected: all 4 existing tests pass (`search_code_rerank_floats_impl_over_leaf_constant`, `search_docs_over_bundled_fixture_returns_ranked_rows`, `search_code_scope_binds_temporal_views_via_icu`, `search_dedups_vendored_copies_and_caps_per_document`) plus the new test.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/code_semantic_search.rs
git commit -m "feat(spur-notebook): W2.2 scope=graph graph-neighborhood retrieval path"
```
