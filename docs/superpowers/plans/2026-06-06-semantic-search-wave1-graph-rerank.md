# Semantic Search Wave 1 — Graph-Aware Rerank + Result Hygiene

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** (none — direct from live evaluation of `code_semantic_search`, 2026-06-06)
**Design epic:** (none — incremental quality fix)

**Goal:** Make `search_code`/`search` rank with the Tier-1/2 graph signals we already
compute (instead of `ORDER BY bm25 DESC`), and stop near-duplicate / single-document
results from flooding the top of the list.

**Architecture:** Pure analyst-SQL change in `init_search.sql`. The scorecard
(`v_symbol_scorecard.pagerank`, `symbol_kind`, `file_path`) is already JOINed into
`search_code`/`search` but only displayed — Wave 1 folds it into the `ORDER BY` and adds a
per-document diversity cap + a normalized cross-copy dedup. No reader/Rust-logic change is
required for ranking; the tool (`code_semantic_search.rs`) just surfaces whatever the macro
returns. Verified by (a) static content guards on `INIT_SEARCH_SQL` and (b) behavioral
fixture tests that mirror the production ordering/dedup expressions on a tiny bundled-duckdb DB.

**Tech Stack:** DuckDB SQL (macros), Rust (`#[test]` fixtures with bundled libduckdb 1.5.x).

**Evidence motivating this (live eval, 2026-06-06):**
- `search_code('worktree orphan sweep cleanup')` ranked the `leaf` constant
  `WORKTREE_CORE_ORPHAN_CLEANUP` (spur-license) **above** the `load-bearing wall`
  implementation `cleanup_orphans` (spur-worktree) — a pure lexical coincidence the graph
  already knows how to break (centrality + kind).
- `search('notebook kernel restart supervisor')` burned result ranks 2–5 on five sections of
  **one** plan document.
- `search('how does the brain approve or reject worker output')` returned 4 near-identical
  `brain-review-gate/SKILL.md` copies (`.spur`/`.claude`/`.codex` + `spur-core`).

**Deploy note (NOT a worker task):** `init_search.sql` is baked into `.spur/analyst.duckdb`
at build time; the live improvement requires an analyst rebuild (`spur-cli graph build` /
analyst build — the SQL fingerprint change forces it). Workers verify via fixtures + static
guards only; the brain triggers the reindex after merge.

---

## File Structure Map

| File | Responsibility | Task |
|---|---|---|
| `crates/spur-context/poc/duckdb-analyst/init_search.sql` | `search_code` + `search` ORDER BY → fused rank | T1 |
| `crates/spur-cli/src/commands/analyst.rs` (tests) | static content guard on `INIT_SEARCH_SQL` rank | T1 |
| `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs` (tests) | behavioral fixture: impl outranks constant | T1 |
| `crates/spur-context/poc/duckdb-analyst/init_search.sql` | per-document cap + normalized cross-copy dedup | T2 |
| `crates/spur-cli/src/commands/analyst.rs` (tests) | static guard on dedup/diversity | T2 |
| `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs` (tests) | behavioral fixture: cap + dedup | T2 |

T2 depends on T1 (both edit `init_search.sql`; sequential stacking avoids a parallel
same-file collision).

---

### Task 1: Graph-aware rerank in `search_code` and `search`

**Task ID:** `task-rerank`

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql:95-104` (`search_code`)
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql:107-128` (`search`, code arm)
- Test: `crates/spur-cli/src/commands/analyst.rs` (new `#[test]` in the existing `mod tests`)
- Test: `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs` (new `#[test]` in `mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `search_code` orders by a fused score: `bm25 × test_penalty × kind_weight × centrality_boost`, not bare `bm25`.
- [ ] The displayed `bm25` column is unchanged (raw BM25, for transparency); only the ordering changes.
- [ ] `search` (unified) applies the same boost to its code arm's ordering key; the doc arm still orders by its BM25.
- [ ] Static guard test passes: `INIT_SEARCH_SQL` no longer contains a bare `ORDER BY bm25 DESC` for `search_code`, and the code ordering references `pagerank` and the `/tests/` penalty.
- [ ] Behavioral fixture test passes: a high-BM25 `constant` ranks **below** a lower-BM25 high-pagerank `function`.
- [ ] `scripts/spur-cargo test -p spur-cli --lib` and `-p spur-notebook --features datasource-introspect code_semantic_search` are green.

**Suggested Worker:** codex (mechanical SQL + fixture test)

**Scope Boundary:**
- IN scope: the two macro `ORDER BY` clauses in `init_search.sql`; the two new test fns.
- OUT of scope: `search_docs`, the corpus tables (`sections_search`/`symbol_text`), `init_views.sql`, any reader/Rust logic in `code_semantic_search.rs` outside its `mod tests`.
- If you discover the reader needs changing, emit `scope_drift` — it should not.

**Implementation:**

- [ ] **Step 1: Write the failing behavioral test** (in `code_semantic_search.rs` `mod tests`).

```rust
// Rerank: a high-BM25 leaf CONSTANT must not outrank a lower-BM25, high-pagerank
// FUNCTION. Mirrors the production search_code ORDER BY against a bundled-duckdb fixture
// (the WORKTREE_CORE_ORPHAN_CLEANUP-over-cleanup_orphans inversion from the live eval).
#[test]
fn search_code_rerank_floats_impl_over_leaf_constant() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("a.duckdb");
    let conn = duckdb::Connection::open(&db)?;
    conn.execute_batch("INSTALL fts; LOAD fts;")?;
    conn.execute_batch(
        r#"
        CREATE TABLE symbol_text(stable_symbol_id VARCHAR, entity_name VARCHAR,
            symbol_kind VARCHAR, file_path VARCHAR, doc_text VARCHAR);
        INSERT INTO symbol_text VALUES
          ('c1','WORKTREE_CORE_ORPHAN_CLEANUP','constant',
           'crates/spur-license/src/policy/feature_key.rs',
           'worktree orphan cleanup worktree orphan cleanup'),
          ('f1','cleanup_orphans','function',
           'crates/spur-worktree/src/manager.rs',
           'worktree orphan cleanup sweep');
        -- scorecard: the function is a load-bearing wall (high pagerank), constant is a leaf.
        CREATE TABLE v_symbol_scorecard(stable_symbol_id VARCHAR, pagerank DOUBLE,
            churn_90d BIGINT, posture VARCHAR, component_size BIGINT);
        INSERT INTO v_symbol_scorecard VALUES
          ('c1', 0.0, 0, 'leaf', 1),
          ('f1', 0.02, 3, 'load-bearing wall', 50);
        "#,
    )?;
    conn.execute_batch(
        "PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text', overwrite=1);",
    )?;
    // The PRODUCTION search_code ORDER BY (kept in sync with init_search.sql).
    conn.execute_batch(
        r#"
        CREATE OR REPLACE MACRO search_code(q) AS TABLE
          SELECT * FROM (
            SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
                   fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                   sc.pagerank
            FROM symbol_text st
            JOIN v_symbol_scorecard sc USING (stable_symbol_id)
            WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
          )
          ORDER BY bm25_raw
            * CASE WHEN file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
            * CASE WHEN symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                   WHEN symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
            * (1 + 0.15 * ln(1 + pagerank * 1e4)) DESC NULLS LAST
          LIMIT 25;
        "#,
    )?;
    let first: String = conn.query_row(
        "SELECT symbol FROM search_code('worktree orphan cleanup') LIMIT 1", [],
        |r| r.get(0))?;
    assert_eq!(first, "cleanup_orphans",
        "the load-bearing function must outrank the leaf constant after rerank");
    Ok(())
}
```

- [ ] **Step 2: Run to verify it fails** (the helper macro doesn't exist / asserts wrong order before you finalize the expression).

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook --features datasource-introspect search_code_rerank_floats_impl_over_leaf_constant -- --nocapture`

- [ ] **Step 3: Rewrite `search_code` in `init_search.sql`** to the fused ordering (raw `bm25` still displayed):

```sql
CREATE OR REPLACE MACRO search_code(q) AS TABLE
  SELECT symbol, symbol_kind, file_path,
         round(bm25_raw, 3) AS bm25,
         round(pagerank * 1e4, 2) AS pagerank_x1e4,
         churn_90d, posture, component_size
  FROM (
    SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
           fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
           sc.pagerank, sc.churn_90d, sc.posture, sc.component_size
    FROM symbol_text st
    JOIN v_symbol_scorecard sc USING (stable_symbol_id)
    WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  )
  ORDER BY bm25_raw
    * CASE WHEN file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
    * CASE WHEN symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
           WHEN symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
    * (1 + 0.15 * ln(1 + pagerank * 1e4)) DESC NULLS LAST
  LIMIT 25;
```

And the **code arm of `search`** — add a `rank` ordering key without changing the displayed
`score` (raw BM25) or the doc arm:

```sql
CREATE OR REPLACE MACRO search(q) AS TABLE
  SELECT kind, title, file, score, signal FROM (
    SELECT 'doc' AS kind, s.qualified_name AS title,
           regexp_replace(s.file_path, '^(crates|docs|\.claude|\.spur|\.codex|\.kiro|\.gemini)/', '') AS file,
           round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS score,
           CAST(NULL AS VARCHAR) AS signal,
           fts_main_sections_search.match_bm25(s.stable_symbol_id, q) AS rank
    FROM sections_search s
    WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
    UNION ALL
    SELECT 'code', st.entity_name, regexp_replace(st.file_path, '^crates/', ''),
           round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3),
           sc.posture || ' · pr=' || round(sc.pagerank * 1e4, 1) || ' · churn=' || sc.churn_90d,
           fts_main_symbol_text.match_bm25(st.stable_symbol_id, q)
             * CASE WHEN st.file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
             * CASE WHEN st.symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                    WHEN st.symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
             * (1 + 0.15 * ln(1 + sc.pagerank * 1e4))
    FROM symbol_text st
    JOIN v_symbol_scorecard sc USING (stable_symbol_id)
    WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  )
  WHERE score IS NOT NULL
  ORDER BY rank DESC NULLS LAST
  LIMIT 30;
```

- [ ] **Step 4: Add the static content guard** (in `analyst.rs` `mod tests`):

```rust
#[test]
fn init_search_sql_ranks_code_with_graph_signals() {
    // search_code/search must fold centrality + kind/test weighting into the ORDER BY,
    // not order by bare bm25 (which let a leaf constant outrank a load-bearing impl).
    assert!(
        !INIT_SEARCH_SQL.contains("ORDER BY bm25 DESC NULLS LAST\n  LIMIT 25"),
        "search_code must not order by bare bm25"
    );
    assert!(
        INIT_SEARCH_SQL.contains("ln(1 + pagerank * 1e4)")
            && INIT_SEARCH_SQL.contains("'%/tests/%'"),
        "code ranking must boost by pagerank and penalize test paths"
    );
}
```

(Confirm `INIT_SEARCH_SQL` is the `include_str!` const in `analyst.rs`; it is declared
alongside `INIT_VIEWS_SQL` — reuse it.)

- [ ] **Step 5: Run both test crates green.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-cli --lib init_search_sql_ranks_code_with_graph_signals`
Run: `scripts/spur-cargo test -p spur-notebook --features datasource-introspect code_semantic_search`

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-context/poc/duckdb-analyst/init_search.sql \
        crates/spur-cli/src/commands/analyst.rs \
        crates/spur-notebook/src/mcp/tools/code_semantic_search.rs
git commit -m "feat(spur-context): rank code search by graph signals not bare bm25"
```

**Scope Drift Checkpoint:**
- If the fused `ORDER BY` won't bind in the bundled duckdb fixture → simplify the expression, do NOT add extensions; emit `risk` if blocked.
- If you need to touch files beyond the three listed → emit `scope_drift`.

---

### Task 2: Per-document diversity cap + normalized cross-copy dedup

**Task ID:** `task-dedup-diversity`

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql` (`search`/`search_docs` cap; `sections_search` dedup key)
- Test: `crates/spur-cli/src/commands/analyst.rs` (new `#[test]`)
- Test: `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs` (new `#[test]`)

**Depends on:** `task-rerank`

**Acceptance Criteria:**
- [ ] `search` and `search_docs` cap results at **2 per `file`** (`QUALIFY row_number() OVER (PARTITION BY file ORDER BY rank/bm25 DESC) <= 2`) so one document can't flood the head.
- [ ] `sections_search` dedup partitions on a **normalized** body that strips the `SPUR-MANAGED` header line, so vendored skill copies that differ only by that header collapse to one canonical (non-dot-dir) row.
- [ ] Static guard test passes: `INIT_SEARCH_SQL` contains the per-file cap and the normalized-dedup `regexp_replace`.
- [ ] Behavioral fixture test passes: (a) 3 vendored copies of one section collapse to 1; (b) ≤2 rows per document survive in a `search`-shaped result.
- [ ] `scripts/spur-cargo test -p spur-cli --lib` and `-p spur-notebook --features datasource-introspect code_semantic_search` are green.

**Suggested Worker:** codex (mechanical SQL + fixture test)

**Scope Boundary:**
- IN scope: `sections_search` table definition; the `search`/`search_docs` macro tails; the two new tests.
- OUT of scope: `search_code` ordering (Task 1 owns it — keep its ORDER BY intact), `symbol_text`, `init_views.sql`, reader/Rust logic.
- If the normalized dedup risks collapsing genuinely-distinct sections → emit `risk` and keep the existing exact-body dedup as a fallback.

**Implementation:**

- [ ] **Step 1: Write the failing behavioral test** (`code_semantic_search.rs` `mod tests`):

```rust
// Diversity + dedup: vendored copies collapse to one, and one document cannot occupy
// more than 2 of the top result slots.
#[test]
fn search_dedups_vendored_copies_and_caps_per_document() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("a.duckdb");
    let conn = duckdb::Connection::open(&db)?;
    conn.execute_batch("INSTALL fts; LOAD fts;")?;
    // sections raw: same skill body vendored 3x (differ only by SPUR-MANAGED header +
    // dot-dir path) plus one 3-section plan document.
    conn.execute_batch(
        r#"
        CREATE TABLE sections(stable_symbol_id VARCHAR, qualified_name VARCHAR,
            file_path VARCHAR, body_text VARCHAR);
        INSERT INTO sections VALUES
          ('a','Brain Review Gate','.claude/skills/brain-review-gate/SKILL.md',
           '<!-- SPUR-MANAGED v=1 sha256=aaa -->\napprove or reject worker output gate'),
          ('b','Brain Review Gate','.codex/skills/brain-review-gate/SKILL.md',
           '<!-- SPUR-MANAGED v=1 sha256=bbb -->\napprove or reject worker output gate'),
          ('c','Brain Review Gate','crates/spur-core/src/skills/brain-review-gate/SKILL.md',
           '<!-- SPUR-MANAGED v=1 sha256=ccc -->\napprove or reject worker output gate'),
          ('p1','Plan::S1','docs/superpowers/plans/p.md','worker output review section one'),
          ('p2','Plan::S2','docs/superpowers/plans/p.md','worker output review section two'),
          ('p3','Plan::S3','docs/superpowers/plans/p.md','worker output review section three');
        "#,
    )?;
    // Normalized dedup: strip the SPUR-MANAGED header line, prefer non-dot-dir path.
    conn.execute_batch(
        r#"
        CREATE OR REPLACE TABLE sections_search AS
        SELECT stable_symbol_id, qualified_name, file_path, body_text
        FROM sections
        QUALIFY row_number() OVER (
          PARTITION BY COALESCE(qualified_name,''),
                       regexp_replace(body_text, '<!-- SPUR-MANAGED[^>]*-->\n?', '')
          ORDER BY (file_path LIKE '.%')::INT, length(file_path), file_path
        ) = 1;
        "#,
    )?;
    // 3 vendored copies must collapse to exactly 1.
    let copies: i64 = conn.query_row(
        "SELECT count(*) FROM sections_search WHERE qualified_name='Brain Review Gate'", [],
        |r| r.get(0))?;
    assert_eq!(copies, 1, "vendored skill copies must dedup to one canonical row");
    conn.execute_batch(
        "PRAGMA create_fts_index('sections_search','stable_symbol_id','body_text', overwrite=1, stemmer='porter');",
    )?;
    // Per-document cap: the 3-section plan must contribute at most 2 rows.
    conn.execute_batch(
        r#"
        CREATE OR REPLACE MACRO search_docs(q) AS TABLE
          SELECT file_path, bm25 FROM (
            SELECT s.file_path,
                   fts_main_sections_search.match_bm25(s.stable_symbol_id, q) AS bm25
            FROM sections_search s
            WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
          )
          QUALIFY row_number() OVER (PARTITION BY file_path ORDER BY bm25 DESC) <= 2
          ORDER BY bm25 DESC NULLS LAST LIMIT 25;
        "#,
    )?;
    let plan_rows: i64 = conn.query_row(
        "SELECT count(*) FROM search_docs('worker output review') WHERE file_path='docs/superpowers/plans/p.md'",
        [], |r| r.get(0))?;
    assert!(plan_rows <= 2, "one document must not exceed 2 result rows");
    Ok(())
}
```

- [ ] **Step 2: Run to verify it fails.**

Run: `scripts/spur-cargo test -p spur-notebook --features datasource-introspect search_dedups_vendored_copies_and_caps_per_document -- --nocapture`

- [ ] **Step 3: Update `sections_search` dedup in `init_search.sql`** (lines 34-40) to the normalized key:

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

- [ ] **Step 4: Add the per-document cap** to `search` (before `ORDER BY rank DESC`) and
`search_docs` (before its `ORDER BY bm25`):

In `search`, wrap the union output:
```sql
  WHERE score IS NOT NULL
  QUALIFY row_number() OVER (PARTITION BY file ORDER BY rank DESC) <= 2
  ORDER BY rank DESC NULLS LAST
  LIMIT 30;
```
In `search_docs`:
```sql
  WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
  QUALIFY row_number() OVER (PARTITION BY s.file_path ORDER BY bm25 DESC) <= 2
  ORDER BY bm25 DESC NULLS LAST
  LIMIT 25;
```

- [ ] **Step 5: Add static guard** (`analyst.rs` `mod tests`):

```rust
#[test]
fn init_search_sql_dedups_and_diversifies() {
    assert!(
        INIT_SEARCH_SQL.contains("SPUR-MANAGED[^>]*-->"),
        "sections_search must dedup on body with the SPUR-MANAGED header stripped"
    );
    assert!(
        INIT_SEARCH_SQL.contains("PARTITION BY file ORDER BY rank DESC) <= 2")
            && INIT_SEARCH_SQL.contains("PARTITION BY s.file_path ORDER BY bm25 DESC) <= 2"),
        "search/search_docs must cap results at 2 per document"
    );
}
```

- [ ] **Step 6: Run both test crates green, then commit.**

```bash
git add crates/spur-context/poc/duckdb-analyst/init_search.sql \
        crates/spur-cli/src/commands/analyst.rs \
        crates/spur-notebook/src/mcp/tools/code_semantic_search.rs
git commit -m "feat(spur-context): dedup vendored sections and cap results per document"
```

**Scope Drift Checkpoint:**
- If the normalized-dedup regex collapses distinct sections in the fixture → keep the exact-body fallback and emit `risk`.
- If `QUALIFY` won't bind in the bundled duckdb fixture → use a `row_number()` subquery filter; do NOT add extensions.

---

## Self-Review

1. **Coverage:** rerank (T1) fixes the leaf-constant inversion; diversity cap (T2) fixes the single-document flood; normalized dedup (T2) fixes the vendored-copy duplication — all three live-eval failures covered.
2. **Placeholders:** none — every macro rewrite and test is concrete.
3. **Type/contract consistency:** T1 introduces the `rank` ordering key in `search`; T2's cap reuses that exact `rank` column. `search_code` displayed columns unchanged; reader (`code_semantic_search.rs`) reads `symbol/file/bm25/posture` — unchanged, so no reader edit.
4. **DAG:** T1 → T2 (linear, no cycle). Sequential because both edit `init_search.sql`; chosen over parallel to avoid a same-file merge collision.
5. **beads:** both tasks have unique IDs (`task-rerank`, `task-dedup-diversity`), explicit `depends_on`, brain-verifiable acceptance criteria, and scope boundaries. Labels stay < 50 chars.

**Post-merge (brain, not worker):** trigger an analyst reindex so the live `code_semantic_search` serves the reranked/deduped results, then re-run the three eval queries to measure lift.
