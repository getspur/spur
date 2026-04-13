# spur-context: DuckDB-Centric Unified Context Engine

**Date**: 2026-04-13
**Status**: Approved
**Crate**: `spur-context`

## Problem

Spur's orchestrator (brain-worker pipeline) is currently stateless across sessions. Each run starts from scratch with no memory of past decisions, outcomes, or learned context. This limits two capabilities:

1. **Agent Memory** — Agents cannot recall past sessions, decisions, or context. There is no structured way to answer "have we seen this before?" or "what worked last time?"
2. **Orchestration Intelligence** — The brain makes delegation decisions without historical data on agent performance, past failure patterns, or accumulated domain knowledge.

## Solution

A new crate `spur-context` providing a unified context engine built on DuckDB with two extensions:

- **DuckPGQ** (Phase 2) — SQL/PGQ graph queries over entity-relationship knowledge graph
- **Lance extension** (Phase 3) — Vector/FTS/hybrid search over embedding datasets

All reads go through DuckDB SQL. Structured writes go through DuckDB. Embedding writes go through the LanceDB Rust crate (native, no FFI). The result is a single query language (SQL) that can join relational, graph, and vector results.

## Design Decisions

### Decision 1: DuckDB from Day One (not SQLite)

**Chosen**: DuckDB even though Phase 1 only has 2 tables.

**Why**: Starting with SQLite and migrating to DuckDB in Phase 2 adds migration tax. DuckDB in Phase 1 validates the Rust crate and build toolchain early. The user requirement is explicitly "DuckDB as unified context engine."

**Trade-off**: Slightly more build complexity (libduckdb C dependency) vs. no future migration cost.

### Decision 2: Single Crate (not split)

**Chosen**: One `spur-context` crate containing all context functionality.

**Why**: Data gravity principle. The brain's queries cross relational, graph, and vector modalities in single operations (e.g., "find similar past decisions and traverse their entity relationships"). Splitting into multiple crates forces these joins into application code instead of SQL.

**Evaluated alternatives**: Technology-split (graph.rs/vector.rs), domain-split (session.rs/knowledge.rs), CQRS (ingest.rs/query.rs). All over-engineer Phase 1's 2-table scope. Structure evolves as phases land.

### Decision 3: Direct Field on Orchestrator (not trait)

**Chosen**: `pub context: Option<ContextEngine>` on the `Orchestrator` struct.

**Why**: Matches the existing `pub cost_tracker: Option<CostTracker>` pattern. Trait-based DI, channel-based decoupling, and middleware wrapping all add abstraction the codebase doesn't use. YAGNI.

### Decision 4: Sync API

**Chosen**: Synchronous methods on `ContextEngine`, caller wraps in `spawn_blocking`.

**Why**: Matches `CostTracker` (sync rusqlite). The DuckDB Rust crate is synchronous. The orchestrator already handles async wrapping for cost tracker calls.

### Decision 5: Separate from spur-cost

**Chosen**: `spur-context` is a new crate alongside `spur-cost`, not a replacement.

**Why**: Different concerns. `spur-cost` tracks accounting (dollars, durations). `spur-context` tracks semantic content (decisions, reasoning, outcomes). They share `session_id` as a common key. Optional future consolidation via DuckDB foreign tables or migration.

### Decision 6: Entity Extraction Deferred to Phase 2

**Chosen**: Phase 1 ships decisions + observations only. Entities and relationships come in Phase 2.

**Why**: Entity extraction is an NLP/LLM task — it's a feature on top of the storage layer, not part of it. Phase 1 delivers the storage foundation. Phase 2 adds the knowledge graph with an extraction pipeline.

## Architecture

```
Orchestrator
  │
  ├── cost_tracker: Option<CostTracker>   (existing, SQLite)
  │
  └── context: Option<ContextEngine>      (new, DuckDB)
        │
        ├── DuckDB Core
        │   ├── decisions table
        │   └── observations table
        │
        ├── DuckPGQ Extension (Phase 2)
        │   ├── entities table
        │   ├── relationships table
        │   └── property graph view
        │
        └── Lance Extension + lancedb crate (Phase 3)
            ├── decision_embeddings.lance
            ├── observation_embeddings.lance
            └── entity_embeddings.lance
```

### Concurrency Model

DuckDB supports concurrent reads but only one writer. This naturally fits Spur's architecture:

- The **orchestrator** (brain) is the single writer — it records decisions and observations after events
- **Workers** report results via ACP; the orchestrator persists them
- Read-only access (brain querying context) is concurrent-safe

No additional concurrency primitives needed.

## Crate Structure

### Phase 1 (Relational Foundation)

```
crates/spur-context/
├── Cargo.toml
└── src/
    ├── lib.rs      # pub mod + re-exports
    ├── engine.rs   # ContextEngine struct, open(), lifecycle
    ├── db.rs       # Schema DDL + write functions + query functions
    └── types.rs    # Decision, Observation, DecisionId structs
```

### Phase 2 Additions (Graph)

```
    ├── graph.rs    # DuckPGQ property graph definition + traversal queries
    # db.rs may split into schema.rs + ingest.rs + query.rs at this point
```

### Phase 3 Additions (Vector)

```
    ├── vector.rs   # Lance dataset management (write via lancedb crate, read via extension)
```

## Data Model

### Phase 1 Schema

```sql
CREATE TABLE IF NOT EXISTS decisions (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    agent       TEXT NOT NULL,
    action      TEXT NOT NULL,
    reasoning   TEXT,
    created_at  TEXT NOT NULL,
    outcome     TEXT NOT NULL DEFAULT 'pending'
);

CREATE TABLE IF NOT EXISTS observations (
    id             TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL,
    decision_id    TEXT NOT NULL,
    agent          TEXT NOT NULL,
    content        TEXT NOT NULL,
    artifacts_json TEXT,
    created_at     TEXT NOT NULL
);
```

- `session_id` links to spur-cost's sessions table by convention (shared UUID), not enforced FK
- `artifacts_json` stores a JSON array of file paths, diffs, or other artifacts; queryable via DuckDB's native JSON functions
- `outcome` tracks decision results: `pending`, `success`, `failure`

### Phase 2 Schema (Knowledge Graph)

```sql
CREATE TABLE IF NOT EXISTS entities (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    entity_type   TEXT NOT NULL,
    properties    TEXT,           -- JSON for extensible metadata
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL,
    mention_count INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS relationships (
    id            TEXT PRIMARY KEY,
    source_entity TEXT NOT NULL,
    target_entity TEXT NOT NULL,
    rel_type      TEXT NOT NULL,
    weight        REAL NOT NULL DEFAULT 1.0,
    session_id    TEXT,
    created_at    TEXT NOT NULL
);

-- DuckPGQ property graph (view over tables, zero data duplication)
CREATE PROPERTY GRAPH knowledge_graph
    VERTEX TABLES (entities PROPERTIES (name, entity_type))
    EDGE TABLES (
        relationships
            SOURCE KEY (source_entity) REFERENCES entities(id)
            DESTINATION KEY (target_entity) REFERENCES entities(id)
            PROPERTIES (rel_type, weight)
    );
```

### Phase 3 Storage (Vector Embeddings)

Lance datasets managed by the `lancedb` Rust crate, queryable via DuckDB's Lance extension:

| Dataset | Key | Vector Dim | Content |
|---------|-----|-----------|---------|
| `decision_embeddings.lance` | decision.id | model-dependent | Embedding of action + reasoning |
| `observation_embeddings.lance` | observation.id | model-dependent | Embedding of content |
| `entity_embeddings.lance` | entity.id | model-dependent | Embedding of entity name + context |

## API Surface

### Rust Types

```rust
pub struct Decision {
    pub id: String,
    pub session_id: String,
    pub agent: String,
    pub action: String,
    pub reasoning: Option<String>,
    pub created_at: String,
    pub outcome: String,
}

pub struct Observation {
    pub id: String,
    pub session_id: String,
    pub decision_id: String,
    pub agent: String,
    pub content: String,
    pub artifacts: Option<Vec<String>>,
    pub created_at: String,
}
```

### ContextEngine API

```rust
pub struct ContextEngine {
    conn: duckdb::Connection,
}

impl ContextEngine {
    /// Open (or create) the context database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self>;

    // ── Writes ──────────────────────────────────────────────

    /// Record a brain decision. Returns the generated decision ID.
    pub fn record_decision(
        &self, session_id: &str, agent: &str,
        action: &str, reasoning: Option<&str>,
    ) -> Result<String>;

    /// Update a decision's outcome after task completion.
    pub fn update_decision_outcome(&self, id: &str, outcome: &str) -> Result<()>;

    /// Record a worker observation. Returns the generated observation ID.
    pub fn record_observation(
        &self, session_id: &str, decision_id: &str,
        agent: &str, content: &str, artifacts: Option<&[String]>,
    ) -> Result<String>;

    // ── Reads ───────────────────────────────────────────────

    /// Return the most recent decisions across all sessions.
    pub fn query_recent_decisions(&self, limit: usize) -> Result<Vec<Decision>>;

    /// Return all decisions for a specific session.
    pub fn query_decisions_for_session(&self, session_id: &str) -> Result<Vec<Decision>>;

    /// Return observations produced by a specific decision.
    pub fn query_observations_for_decision(&self, decision_id: &str) -> Result<Vec<Observation>>;

    /// Full-text keyword search over decision actions and reasoning.
    pub fn search_decisions(&self, keyword: &str, limit: usize) -> Result<Vec<Decision>>;
}
```

### Phase 2 API Additions

```rust
impl ContextEngine {
    /// Record an extracted entity.
    pub fn record_entity(&self, name: &str, entity_type: &str, properties: Option<&str>) -> Result<String>;

    /// Record a relationship between two entities.
    pub fn record_relationship(
        &self, source: &str, target: &str, rel_type: &str,
        weight: f32, session_id: Option<&str>,
    ) -> Result<String>;

    /// Traverse the knowledge graph from a named entity to a given depth.
    pub fn query_related_entities(
        &self, entity_name: &str, depth: usize,
    ) -> Result<Vec<(Entity, Relationship)>>;

    /// Find the shortest decision chain between two decisions.
    pub fn query_decision_chain(
        &self, from_id: &str, to_id: &str,
    ) -> Result<Vec<Decision>>;
}
```

### Phase 3 API Additions

```rust
impl ContextEngine {
    /// Find decisions semantically similar to the given embedding.
    pub fn recall_similar_decisions(
        &self, embedding: &[f32], top_k: usize,
    ) -> Result<Vec<(Decision, f32)>>;

    /// Find observations semantically similar to the given embedding.
    pub fn recall_similar_observations(
        &self, embedding: &[f32], top_k: usize,
    ) -> Result<Vec<(Observation, f32)>>;

    /// Hybrid recall combining vector similarity and keyword matching.
    pub fn hybrid_recall(
        &self, embedding: &[f32], keywords: &str, top_k: usize,
    ) -> Result<Vec<RecallResult>>;
}
```

## Phased Delivery

| Phase | Scope | DuckDB Feature | Fallback | Validation Gate |
|-------|-------|----------------|----------|-----------------|
| **1** | decisions + observations tables, basic CRUD + search | Core DuckDB only | N/A | DuckDB Rust crate builds and runs in Spur's CI |
| **2** | entities + relationships, DuckPGQ property graph, graph traversal | DuckPGQ community extension | Recursive CTEs with `USING KEY` | Extension loads from Rust, property graphs persist |
| **3** | Lance datasets, embedding storage, vector/FTS/hybrid search | Lance core extension + lancedb Rust crate | lancedb crate directly (join in app code) | Lance files written by lancedb are readable by DuckDB extension |

Each phase is independently valuable and validates its toolchain before the next proceeds.

## Dependencies

### Workspace Cargo.toml Additions

```toml
# Phase 1
duckdb = { version = "1", features = ["bundled"] }

# Phase 3 (deferred)
# lancedb = "0.15"
# arrow = "53"
```

### spur-context/Cargo.toml

```toml
[package]
name = "spur-context"
description = "DuckDB-backed unified context engine — decisions, knowledge graph, and semantic recall"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
duckdb = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
```

### spur-core/Cargo.toml Addition

```toml
spur-context = { path = "../spur-context" }
```

## Query Examples

### Phase 1: Relational

```sql
-- Brain asks: "What did we decide in the last 5 sessions?"
SELECT d.*, o.content AS outcome_detail
FROM decisions d
LEFT JOIN observations o ON o.decision_id = d.id
ORDER BY d.created_at DESC
LIMIT 20;

-- Brain asks: "Any past decisions about authentication?"
SELECT * FROM decisions
WHERE action ILIKE '%auth%' OR reasoning ILIKE '%auth%'
ORDER BY created_at DESC;
```

### Phase 2: Graph

```sql
-- Brain asks: "What concepts are related to 'JWT'?"
SELECT dst.name, e.rel_type, e.weight
FROM GRAPH_TABLE(knowledge_graph
    MATCH (src:entities)-[e:relationships]->{1,3}(dst:entities)
    WHERE src.name = 'JWT'
    COLUMNS (dst.name, e.rel_type, e.weight)
)
ORDER BY e.weight DESC;
```

### Phase 3: Vector + Graph Unified

```sql
-- Brain asks: "Find similar past decisions AND their related entities"
WITH similar AS (
    SELECT id, action, reasoning, score
    FROM lance_vector_search('decision_embeddings.lance', ?embedding, 10)
),
related AS (
    SELECT s.id AS decision_id, dst.name AS entity, r.rel_type
    FROM similar s
    JOIN observations o ON o.decision_id = s.id
    JOIN entities src ON o.content ILIKE '%' || src.name || '%'
    JOIN GRAPH_TABLE(knowledge_graph
        MATCH (src_node:entities)-[r:relationships]->(dst:entities)
        WHERE src_node.id = src.id
        COLUMNS (src_node.id AS src_id, dst.name, r.rel_type)
    ) g ON g.src_id = src.id
)
SELECT * FROM similar s LEFT JOIN related r ON r.decision_id = s.id;
```

## Risk Mitigation

| Risk | Severity | Mitigation |
|------|----------|------------|
| DuckPGQ is research-stage | Moderate | Fallback to recursive CTEs; graph is a query syntax layer, not storage |
| DuckDB Rust crate extension loading | Low | Extensions load via SQL commands; validated in Phase 2 gate |
| Lance/lancedb version compatibility | Low | Both use Lance format; pin compatible versions |
| DuckDB single-writer constraint | None | Orchestrator is naturally the single writer; workers report via ACP |
| Unbounded knowledge graph growth | None | ~18M entities/year at aggressive usage; DuckDB handles 100M+ trivially |
| Build complexity (C dependency) | Low | `bundled` feature compiles from source; same pattern as rusqlite |
