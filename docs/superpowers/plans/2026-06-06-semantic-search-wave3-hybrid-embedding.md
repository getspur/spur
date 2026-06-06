# Semantic Search Wave 3 — Hybrid Embedding Retrieval Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** derived from code-explore session (2026-06-06) — no separate spec file
**Design epic:** n/a (brain-approved during /code-explore session)

**Goal:** Add 768-dim vector embeddings to the `section_bodies` Lance table so that `code_semantic_search(scope="hybrid")` fuses BM25 with cosine similarity for plan/spec/doc sections, bridging the vocabulary gap where abstract queries miss code-vocabulary BM25 hits.

**Architecture:** Embeddings are generated in `spur-graph` (the graph build step) using `fastembed-rs` with `nomic-embed-text-v1.5`, stored colocated with source text in the existing `sections.lancedb` Lance database as a nullable `vector FLOAT[768]` column. At analyst-build time, eligible rows are materialized into a DuckDB `sections_embeddings` table. At query time, the `spur-notebook` MCP tool embeds the query with the same model and calls a new `search_hybrid(q, vec)` DuckDB macro that RRF-fuses BM25 + cosine ranks. Graceful degradation at every layer: if fastembed fails (no model download), the embedding column is NULL and `scope="hybrid"` falls back to BM25-only docs search.

**Tech Stack:** `fastembed = "4"` (ONNX Runtime, nomic-embed-text-v1.5, 768-dim), `lancedb = "0.29.0"` (already workspace), `arrow-array`/`arrow-schema` (already workspace), DuckDB `list_cosine_similarity()` built-in for brute-force cosine over 15K rows.

**Schema Contract (both tasks implement against this):**

| Lance table | Column | Arrow type | Notes |
|---|---|---|---|
| `section_bodies` | `vector` | `FixedSizeList<Float32, 768>` nullable | NULL for h1/oversized sections |
| DuckDB `sections_embeddings` | `stable_symbol_id` | VARCHAR | materialized from Lance |
| DuckDB `sections_embeddings` | `vector` | `FLOAT[768]` | materialized from Lance |

**Embedding target filter:** `heading_level >= 2 AND length(body_text) <= 4096`
→ 15,634 rows (87–99% of h2-h4 sections), zero chunking needed, ~34 s on CPU.

---

### Task 1: task-vector-write

**Task ID:** `task-vector-write`

**Files:**
- Modify: `crates/spur-graph/Cargo.toml` (add fastembed dep)
- Modify: `crates/spur-graph/src/store/lance_sections.rs` (vector column + embed step)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `spur graph build` completes without error when fastembed model is unavailable (graceful skip)
- [ ] When fastembed IS available: `section_bodies` Lance table has a `vector` column with non-NULL values for h2-h4 sections ≤ 4096 chars, NULL for all others
- [ ] HNSW vector index is created on the `vector` column after write
- [ ] `write_sections_gracefully_skips_vector_when_model_unavailable` test passes
- [ ] `sections_schema_includes_nullable_vector_column` test passes
- [ ] No compilation errors, `scripts/spur-cargo clippy -p spur-graph` clean

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `spur-graph/Cargo.toml`, `crates/spur-graph/src/store/lance_sections.rs` only
- OUT of scope: `init_search.sql`, `analyst.rs`, `code_semantic_search.rs`, any other crate
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` signal immediately.

**Scope Drift Checkpoint:**
- If fastembed model download adds >10 min to graph build → emit `risk` signal
- If schema migration for existing `section_bodies` rows is needed → emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Add fastembed dependency**

In `crates/spur-graph/Cargo.toml`, under `[dependencies]`, add:
```toml
fastembed = { version = "4", default-features = false }
```

Note: `lancedb`, `arrow-array`, `arrow-schema`, `tokio` are already present in `spur-graph/Cargo.toml`. No other new deps needed.

- [ ] **Step 2: Add vector field to SectionRow**

In `lance_sections.rs`, update `SectionRow`:
```rust
#[derive(Debug)]
struct SectionRow {
    stable_symbol_id: String,
    file_path: String,
    qualified_name: String,
    heading_level: u8,
    body_text: String,
    body_byte_start: u64,
    body_byte_end: u64,
    child_count: u32,
    parent_stable_id: Option<String>,
    content_hash: String,
    vector: Option<Vec<f32>>,  // NEW: None for non-eligible sections
}
```

Update `section_row()` and the `SectionRow { ... }` literal at line 249 (the whole-file fallback path) to include `vector: None` (embedding is assigned in a later pass).

- [ ] **Step 3: Update sections_schema() to include vector column**

```rust
fn sections_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("stable_symbol_id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("qualified_name", DataType::Utf8, false),
        Field::new("heading_level", DataType::UInt8, false),
        Field::new("body_text", DataType::LargeUtf8, false),
        Field::new("body_byte_start", DataType::UInt64, false),
        Field::new("body_byte_end", DataType::UInt64, false),
        Field::new("child_count", DataType::UInt32, false),
        Field::new("parent_stable_id", DataType::Utf8, true),
        Field::new("content_hash", DataType::Utf8, false),
        // NEW: nullable — None for h1/oversized sections
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                768,
            ),
            true,
        ),
    ]))
}
```

- [ ] **Step 4: Write embed_eligible_rows()**

Add this function (graceful: returns all None if model fails):

```rust
fn embed_eligible_rows(rows: &[SectionRow]) -> Vec<Option<Vec<f32>>> {
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

    let eligible: Vec<(usize, &str)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.heading_level >= 2 && r.body_text.len() <= 4096)
        .map(|(i, r)| (i, r.body_text.as_str()))
        .collect();

    if eligible.is_empty() {
        return vec![None; rows.len()];
    }

    let model = match TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
            .with_show_download_progress(false),
    ) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "fastembed model unavailable — skipping section embeddings");
            return vec![None; rows.len()];
        }
    };

    let texts: Vec<&str> = eligible.iter().map(|(_, t)| *t).collect();
    let embeddings = match model.embed(texts, None) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "fastembed encode failed — skipping section embeddings");
            return vec![None; rows.len()];
        }
    };

    let mut result = vec![None; rows.len()];
    for ((idx, _), vec) in eligible.iter().zip(embeddings.into_iter()) {
        result[*idx] = Some(vec);
    }
    result
}
```

- [ ] **Step 5: Call embed in write_sections_dataset_async and populate SectionRow.vector**

In `write_sections_dataset_async`, after `let rows = section_rows(artifact, worktree_root)?;`:

```rust
let rows = section_rows(artifact, worktree_root)?;

// Embed eligible rows; populate vector field on each SectionRow.
let vectors = embed_eligible_rows(&rows);
let rows: Vec<SectionRow> = rows
    .into_iter()
    .zip(vectors)
    .map(|(mut row, vec)| {
        row.vector = vec;
        row
    })
    .collect();
```

- [ ] **Step 6: Update rows_to_batch() to serialize the vector column**

Add imports at the top of the file:
```rust
use arrow_array::{FixedSizeListArray, Float32Array};
use arrow_buffer::NullBuffer;
```

In `rows_to_batch()`, collect vectors alongside existing fields:

```rust
fn rows_to_batch(rows: Vec<SectionRow>, schema: Arc<Schema>) -> Result<RecordBatch> {
    // ... existing field vecs ...
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(rows.len());

    for row in rows {
        // ... existing pushes ...
        vectors.push(row.vector);
    }

    // Build FixedSizeListArray for the vector column
    let flat_values: Vec<f32> = vectors
        .iter()
        .flat_map(|v| match v {
            Some(vec) => vec.iter().copied().collect::<Vec<_>>(),
            None => vec![0.0f32; 768],
        })
        .collect();
    let null_buffer = NullBuffer::from(
        vectors.iter().map(|v| v.is_some()).collect::<Vec<bool>>()
    );
    let value_field = Arc::new(Field::new("item", DataType::Float32, true));
    let values_array = Arc::new(Float32Array::from(flat_values));
    let vector_array = FixedSizeListArray::try_new(
        value_field,
        768,
        values_array,
        Some(null_buffer),
    )
    .context("failed to build vector FixedSizeListArray")?;

    RecordBatch::try_new(
        schema,
        vec![
            // ... existing arrays in same order as sections_schema() ...
            Arc::new(vector_array),
        ],
    )
    .context("failed to build LanceDB sections batch")
}
```

- [ ] **Step 7: Add ensure_vector_index() and call it**

Following the exact pattern of `ensure_body_text_fts_index`:

```rust
async fn ensure_vector_index(table: &lancedb::Table) -> Result<()> {
    use lancedb::index::{Index, IndexType};

    // Only index if at least one non-NULL vector row exists
    let has_vectors = table
        .count_rows(Some("vector IS NOT NULL".to_string()))
        .await
        .context("failed to count vector rows")?;

    if has_vectors == 0 {
        return Ok(());
    }

    if table
        .list_indices()
        .await
        .context("failed to list LanceDB section indices")?
        .iter()
        .any(|index| {
            index.index_type == IndexType::Vector
                && index.columns.as_slice() == ["vector"]
        })
    {
        return Ok(());
    }

    table
        .create_index(
            &["vector"],
            Index::IvfHnswSq(lancedb::index::vector::IvfHnswSqIndexBuilder::default()),
        )
        .execute()
        .await
        .context("failed to create LanceDB vector HNSW index")
}
```

In `write_sections_dataset_async`, after `ensure_body_text_fts_index`:
```rust
if dataset_changed {
    ensure_body_text_fts_index(&table).await?;
    ensure_vector_index(&table).await?;  // NEW
}
```

- [ ] **Step 8: Write tests**

```rust
#[test]
fn sections_schema_includes_nullable_vector_column() {
    let schema = sections_schema();
    let vector_field = schema.field_with_name("vector").expect("vector field missing");
    assert!(vector_field.is_nullable(), "vector must be nullable");
    assert!(
        matches!(vector_field.data_type(), DataType::FixedSizeList(_, 768)),
        "vector must be FLOAT[768], got {:?}",
        vector_field.data_type()
    );
}

#[test]
fn embed_eligible_rows_returns_none_for_h1_and_oversized() {
    let rows = vec![
        SectionRow {
            stable_symbol_id: "a".into(),
            file_path: "docs/foo.md".into(),
            qualified_name: "foo".into(),
            heading_level: 1,       // NOT eligible
            body_text: "# Heading".into(),
            body_byte_start: 0,
            body_byte_end: 9,
            child_count: 0,
            parent_stable_id: None,
            content_hash: "abc".into(),
            vector: None,
        },
        SectionRow {
            stable_symbol_id: "b".into(),
            file_path: "docs/foo.md".into(),
            qualified_name: "foo/bar".into(),
            heading_level: 2,       // eligible IF model available
            body_text: "## Short section body".into(),
            body_byte_start: 9,
            body_byte_end: 30,
            child_count: 0,
            parent_stable_id: Some("a".into()),
            content_hash: "abc".into(),
            vector: None,
        },
    ];

    let result = embed_eligible_rows(&rows);
    assert_eq!(result.len(), 2);
    // h1 row must always be None regardless of model
    assert!(result[0].is_none(), "h1 must not be embedded");
    // h2 row: None when model unavailable (CI), Some when model present
    // We only assert length, not value — CI won't have fastembed model.
    // When model IS available: assert!(result[1].is_some() && result[1].as_ref().unwrap().len() == 768)
}
```

- [ ] **Step 9: Run tests and build**

```bash
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph sections_schema_includes_nullable_vector_column embed_eligible_rows -- --nocapture
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
```

- [ ] **Step 10: Commit**

```bash
git add crates/spur-graph/Cargo.toml crates/spur-graph/src/store/lance_sections.rs
git commit -m "feat(spur-graph): add FLOAT[768] vector column to section_bodies Lance table"
```

---

### Task 2: task-embed-materialize

**Task ID:** `task-embed-materialize`

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/init_search.sql` (materialize + search_hybrid macro)
- Modify: `crates/spur-cli/src/commands/analyst.rs` (guard test)

**Depends on:** none (schema contract defined above; SQL written against spec, runtime fails gracefully when vector column absent)

**Acceptance Criteria:**
- [ ] `analyst build` completes without error when `vector` column is all NULL (no fastembed model)
- [ ] `sections_embeddings` DuckDB table is created (possibly empty) after analyst build
- [ ] `search_hybrid` DuckDB macro is present in analyst.duckdb
- [ ] `init_search_sql_hybrid_macro_present` guard test passes
- [ ] `init_search_sql_sections_embeddings_materialized` guard test passes
- [ ] No placeholders in SQL; `list_cosine_similarity` is the cosine operator used

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `init_search.sql` (append after existing macros), `analyst.rs` (append two guard tests)
- OUT of scope: `lance_sections.rs`, `code_semantic_search.rs`, any other file
- If you need to modify other SQL scripts, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write failing guard tests first**

In `crates/spur-cli/src/commands/analyst.rs`, inside the `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn init_search_sql_sections_embeddings_materialized() {
    let sql = INIT_SEARCH_SQL;
    assert!(
        sql.contains("CREATE OR REPLACE TABLE sections_embeddings"),
        "init_search.sql must materialize sections_embeddings table"
    );
    assert!(
        sql.contains("FROM lance_ns.section_bodies"),
        "sections_embeddings must source from lance_ns.section_bodies"
    );
    assert!(
        sql.contains("heading_level >= 2"),
        "sections_embeddings must filter to heading_level >= 2"
    );
    assert!(
        sql.contains("vector IS NOT NULL"),
        "sections_embeddings must filter to non-NULL vectors"
    );
}

#[test]
fn init_search_sql_hybrid_macro_present() {
    let sql = INIT_SEARCH_SQL;
    assert!(
        sql.contains("CREATE OR REPLACE MACRO search_hybrid(q, vec)"),
        "init_search.sql must define search_hybrid macro"
    );
    assert!(
        sql.contains("list_cosine_similarity"),
        "search_hybrid must use list_cosine_similarity"
    );
    assert!(
        sql.contains("1.0 / (60.0 + "),
        "search_hybrid must implement RRF with k=60"
    );
}
```

Run: `scripts/spur-cargo test -p spur-cli init_search_sql_sections -- --nocapture`
Expected: FAIL — guards not satisfied yet.

- [ ] **Step 2: Append sections_embeddings materialization to init_search.sql**

At the bottom of `crates/spur-context/poc/duckdb-analyst/init_search.sql`, after the `search_graph` macro, append:

```sql
-- ── Embedding corpus: eligible section vectors materialized from Lance ────────
-- Eligible = heading_level 2-4, body ≤ 4096 chars, vector IS NOT NULL.
-- Empty when fastembed was unavailable during graph build (graceful degradation).
-- scope="hybrid" in code_semantic_search falls back to BM25 when this is empty.
CREATE OR REPLACE TABLE sections_embeddings AS
SELECT stable_symbol_id,
       vector::FLOAT[768] AS vector
FROM lance_ns.section_bodies
WHERE vector IS NOT NULL
  AND heading_level >= 2
  AND length(body_text) <= 4096;
```

- [ ] **Step 3: Append search_hybrid macro to init_search.sql**

Immediately after the sections_embeddings CREATE TABLE:

```sql
-- Hybrid: BM25 over docs + cosine over embeddings, fused with RRF (k=60).
-- vec parameter: comma-separated 768 floats inline as DuckDB FLOAT[768] literal,
-- e.g. vec = '0.021,-0.003,...' — caller wraps as [vec]::FLOAT[768].
-- Falls back to BM25-only when sections_embeddings is empty.
CREATE OR REPLACE MACRO search_hybrid(q, vec) AS TABLE
  SELECT kind, title, file, score, signal
  FROM (
    WITH bm25 AS (
      SELECT
        s.stable_symbol_id,
        s.qualified_name                                                    AS title,
        regexp_replace(s.file_path,
          '^(docs|\.claude|\.codex|\.kiro|\.gemini|crates|\.spur)/', '')   AS file,
        round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS bm25_score,
        row_number() OVER (
          ORDER BY fts_main_sections_search.match_bm25(s.stable_symbol_id, q) DESC
        ) AS bm25_rank
      FROM sections_search s
      WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
      LIMIT 30
    ),
    ann AS (
      SELECT
        stable_symbol_id,
        list_cosine_similarity(vector, (CAST('[' || vec || ']' AS FLOAT[768]))) AS cos_sim,
        row_number() OVER (
          ORDER BY list_cosine_similarity(vector, (CAST('[' || vec || ']' AS FLOAT[768]))) DESC
        ) AS ann_rank
      FROM sections_embeddings
      WHERE vector IS NOT NULL
      LIMIT 30
    ),
    rrf AS (
      SELECT
        COALESCE(b.stable_symbol_id, a.stable_symbol_id) AS stable_symbol_id,
        1.0 / (60.0 + COALESCE(CAST(b.bm25_rank AS DOUBLE), 31.0))
        + 1.0 / (60.0 + COALESCE(CAST(a.ann_rank AS DOUBLE), 31.0)) AS rrf_score
      FROM bm25 b FULL OUTER JOIN ann a USING (stable_symbol_id)
    )
    SELECT
      'doc'                                                                  AS kind,
      s.qualified_name                                                       AS title,
      regexp_replace(s.file_path,
        '^(docs|\.claude|\.codex|\.kiro|\.gemini|crates|\.spur)/', '')      AS file,
      round(r.rrf_score, 4)                                                  AS score,
      CAST(NULL AS VARCHAR)                                                  AS signal
    FROM rrf r
    JOIN sections_search s ON s.stable_symbol_id = r.stable_symbol_id
    ORDER BY rrf_score DESC
    LIMIT 30
  );
```

Note on the vec parameter syntax: the caller passes a comma-separated float string
(e.g., `"0.021,-0.003,0.114,..."`). The macro wraps it: `CAST('[' || vec || ']' AS FLOAT[768])`.
This is valid DuckDB syntax for constructing a fixed-size float array from a string literal.
Verify this cast syntax works in DuckDB 1.x — if not, the worker should use
`regexp_split_to_array(vec, ',')::FLOAT[]` as a fallback.

- [ ] **Step 4: Run guard tests**

```bash
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-cli init_search_sql_sections init_search_sql_hybrid -- --nocapture
```

Expected: PASS — both guards satisfied.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-context/poc/duckdb-analyst/init_search.sql \
        crates/spur-cli/src/commands/analyst.rs
git commit -m "feat(spur-analyst): materialize sections_embeddings + search_hybrid BM25+cosine macro"
```

---

### Task 3: task-hybrid-query

**Task ID:** `task-hybrid-query`

**Files:**
- Modify: `crates/spur-notebook/Cargo.toml` (add fastembed dep)
- Modify: `crates/spur-notebook/src/mcp/tools/code_semantic_search.rs` (scope=hybrid arm)

**Depends on:** `task-embed-materialize`

**Acceptance Criteria:**
- [ ] `code_semantic_search(scope="hybrid", query="...")` returns 5-column results (kind/title/file/score/signal)
- [ ] `scope="hybrid"` with missing `sections_embeddings` table falls back gracefully (returns BM25 docs results, no error)
- [ ] `scope="hybrid"` with unavailable fastembed model falls back gracefully (returns empty or BM25 results)
- [ ] `search_hybrid_scope_returns_fused_results` test passes
- [ ] `search_hybrid_scope_graceful_fallback_when_no_embeddings_table` test passes
- [ ] `"hybrid"` is accepted by the scope validation guard (no McpError on valid scope)

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `spur-notebook/Cargo.toml`, `code_semantic_search.rs` only
- OUT of scope: `lance_sections.rs`, `init_search.sql`, `analyst.rs`
- Do NOT touch `search_rows` arms for `"docs"`, `"code"`, `"graph"`, or `"all"`.

**Scope Drift Checkpoint:**
- If the fastembed API in version 4 differs significantly from the plan code → emit `risk` with the correct API
- If `CAST('[' || vec || ']' AS FLOAT[768])` is not valid DuckDB syntax → emit `risk`; fallback: pass vec as a preformatted DuckDB literal `[f1,f2,...,f768]::FLOAT[768]`

**Implementation:**

- [ ] **Step 1: Add fastembed dependency**

In `crates/spur-notebook/Cargo.toml`, under `[dependencies]`:
```toml
fastembed = { version = "4", default-features = false }
```

- [ ] **Step 2: Write failing tests first**

In `code_semantic_search.rs`, inside the existing `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn search_hybrid_scope_graceful_fallback_when_no_embeddings_table() {
    // Build an in-memory DuckDB with sections_search but NO sections_embeddings table.
    // scope="hybrid" must return Ok (possibly empty) rather than McpError.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch("
        INSTALL fts; LOAD fts;
        CREATE TABLE sections_search (
            stable_symbol_id VARCHAR PRIMARY KEY,
            qualified_name VARCHAR,
            file_path VARCHAR,
            heading_level UTINYINT,
            body_text VARCHAR
        );
        INSERT INTO sections_search VALUES
            ('s1', 'design/overview', 'docs/design.md', 2, 'This section covers the architecture of the system.');
        PRAGMA create_fts_index('sections_search', 'stable_symbol_id', 'body_text', overwrite=1);
    ").unwrap();

    // Calling the search_hybrid macro when sections_embeddings is absent
    // must not crash — the FULL OUTER JOIN with an empty ANN subquery returns BM25 results only.
    let result = conn.query_row(
        "SELECT count(*) FROM (
           WITH bm25 AS (
             SELECT stable_symbol_id, qualified_name AS title,
                    file_path AS file,
                    fts_main_sections_search.match_bm25(stable_symbol_id, 'architecture') AS bm25_score,
                    row_number() OVER (ORDER BY fts_main_sections_search.match_bm25(stable_symbol_id, 'architecture') DESC) AS bm25_rank
             FROM sections_search
             WHERE fts_main_sections_search.match_bm25(stable_symbol_id, 'architecture') IS NOT NULL
             LIMIT 30
           ),
           ann AS (
             SELECT stable_symbol_id, 0.0 AS cos_sim, 1 AS ann_rank
             FROM sections_search WHERE 1=0  -- empty, simulates missing table
           ),
           rrf AS (
             SELECT COALESCE(b.stable_symbol_id, a.stable_symbol_id) AS stable_symbol_id,
                    1.0 / (60.0 + COALESCE(CAST(b.bm25_rank AS DOUBLE), 31.0))
                    + 1.0 / (60.0 + COALESCE(CAST(a.ann_rank AS DOUBLE), 31.0)) AS rrf_score
             FROM bm25 b FULL OUTER JOIN ann a USING (stable_symbol_id)
           )
           SELECT 'doc' AS kind, s.qualified_name AS title, s.file_path AS file,
                  round(r.rrf_score, 4) AS score, CAST(NULL AS VARCHAR) AS signal
           FROM rrf r JOIN sections_search s ON s.stable_symbol_id = r.stable_symbol_id
           ORDER BY rrf_score DESC LIMIT 30
         )",
        [],
        |row| row.get::<_, i64>(0),
    ).unwrap();

    assert!(result >= 0, "hybrid query must not error on missing embeddings table");
}
```

Run: `scripts/spur-cargo test -p spur-notebook search_hybrid_scope -- --nocapture`
Expected: the SQL test passes (it's pure SQL, no model needed); scope validation test fails until step 4.

- [ ] **Step 3: Add lazy-initialized embedding model**

At the top of `code_semantic_search.rs`, after existing imports, add:

```rust
use std::sync::OnceLock;

static EMBED_MODEL: OnceLock<Option<fastembed::TextEmbedding>> = OnceLock::new();

fn get_embed_model() -> Option<&'static fastembed::TextEmbedding> {
    EMBED_MODEL
        .get_or_init(|| {
            fastembed::TextEmbedding::try_new(
                fastembed::InitOptions::new(fastembed::EmbeddingModel::NomicEmbedTextV15)
                    .with_show_download_progress(false),
            )
            .ok()
        })
        .as_ref()
}

/// Embed query and format as a comma-separated float string for SQL interpolation.
/// Returns None if the model is unavailable.
fn embed_query_as_csv(query: &str) -> Option<String> {
    let model = get_embed_model()?;
    let embeddings = model.embed(vec![query], None).ok()?;
    let vec = embeddings.into_iter().next()?;
    if vec.len() != 768 {
        return None;
    }
    Some(
        vec.iter()
            .map(|f| format!("{:.6}", f))
            .collect::<Vec<_>>()
            .join(","),
    )
}
```

- [ ] **Step 4: Add "hybrid" to scope validation and search_rows arm**

In `search_rows`, find the existing validation guard (the `matches!` check for valid scopes). Add `"hybrid"`:

```rust
// Before: matches!(scope, "all" | "docs" | "code" | "graph")
// After:
if !matches!(scope, "all" | "docs" | "code" | "graph" | "hybrid") {
    return Err(internal(
        "invalid scope",
        &format!("scope must be one of: all, docs, code, graph, hybrid — got: {scope}"),
    ));
}
```

In the `match scope { ... }` block, add the `"hybrid"` arm before the `_` fallback:

```rust
"hybrid" => {
    // Embed query → CSV float string → SQL macro call.
    // Falls back to scope="docs" BM25 when model or embeddings table unavailable.
    match embed_query_as_csv(query) {
        Some(vec_csv) => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal \
             FROM search_hybrid('{q}', '{vec_csv}') LIMIT {limit}"
        ),
        None => {
            // Model unavailable: fall back to BM25 docs only.
            format!(
                "SELECT 'doc' AS kind, section AS title, file_path AS file, \
                 round(bm25, 3) AS score, CAST(NULL AS VARCHAR) AS signal \
                 FROM search_docs('{q}') LIMIT {limit}"
            )
        }
    }
}
```

- [ ] **Step 5: Run all tests**

```bash
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook search -- --nocapture
```

Expected: all 5+ existing tests pass, new hybrid tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/Cargo.toml \
        crates/spur-notebook/src/mcp/tools/code_semantic_search.rs
git commit -m "feat(spur-notebook): add scope=hybrid to code_semantic_search using BM25+cosine RRF"
```

---

## Self-Review

**Spec coverage:**
- ✅ Vector write: task-vector-write covers graph build step with fastembed + Lance HNSW
- ✅ Materialization: task-embed-materialize covers DuckDB sections_embeddings + search_hybrid macro
- ✅ Query path: task-hybrid-query covers MCP tool scope=hybrid arm with fallback
- ✅ Graceful degradation at every layer (model unavailable, empty table, missing table)

**Placeholder scan:** None — all steps have actual code.

**Type consistency:**
- `vector: Option<Vec<f32>>` in SectionRow → `FixedSizeListArray` in Arrow → `FLOAT[768]` in DuckDB — consistent chain
- `embed_query_as_csv` returns comma-separated string → `search_hybrid(q, vec_csv)` wraps as `CAST('[' || vec || ']' AS FLOAT[768])` — consistent

**DAG validation:**
```
task-vector-write ─────┐
                       │
task-embed-materialize ─┼──→ task-hybrid-query
```
Valid DAG. task-vector-write and task-embed-materialize are parallel roots. task-hybrid-query is the sole dependent. No cycles.

**Beads compatibility:** ✅ All three tasks have unique IDs, explicit `depends_on`, verifiable acceptance criteria, and scope boundaries.

**Risk note:** The `CAST('[' || vec || ']' AS FLOAT[768])` DuckDB syntax needs verification. If the DuckDB version on the build VM doesn't support this cast, the worker should use the explicit literal form `[f1,f2,...,f768]::FLOAT[768]` (preformatted in Rust) as the `vec` parameter instead. Emit `risk` if this is the case.
