# spur-graph

Multi-language **code-graph substrate** for SPUR: tree-sitter extraction, content-addressed stable IDs, incremental rebuild, Parquet artifacts, and the exact `code_*` MCP query surface.

`spur-graph` is the **hot / exact tier** of worktree code intelligence. It turns a git worktree into a typed fact graph (`GraphIndexArtifact`) that answers “where is X”, “what calls X”, “what changed”, and “is this answer fresh”.

It does **not** do ranked SQL analytics or evidence packs — that is `spur-analyst` over DuckDB. Agents and brains consume this crate’s facts through MCP (`code_*` tools); they do not normally load Parquet directly.

## Videos

### Four-beat explainer
[spur-graph-explainer.webm](https://github.com/user-attachments/assets/9c9f0999-bf13-4716-be1c-9d889db62ced)

<p align="center">
  <sub>
    <a href="./assets/spur-graph-explainer.webm">Watch the 48-second WebM explainer</a>
    — files become normalized facts, stable identities, incremental rebuilds, and freshness-aware <code>code_*</code> answers.
  </sub>
</p>

### Graph-first code exploration

[spur-graph-explore-code.webm](https://github.com/user-attachments/assets/95ed7a64-d8ce-444f-8cf5-b9db6890fae9)


<p align="center">
  <sub>
    <a href="./assets/spur-graph-explore-code.webm">Watch the 33-second WebM demo</a>
    — a Grok brain uses graph-backed code exploration to resolve current source, follow consumers, and assemble a concrete impact model.
  </sub>
</p>

```
Worktree + git (gix)
        │
        ▼
  discovery → tree-sitter extract → stable IDs → GraphIndexArtifact
        │                                      │
        │                                      ├─ Parquet + pointer  ─► spur-analyst (warm)
        │                                      └─ GraphQueryClient
        │                                              │
        └─ OverlayClient (dirty delta) ◄───────────────┤
                                                       ▼
                                              GraphMcpModule (code_*)
                                                       │
                                                       ▼
                                              Brain / workers
```

> **Deep dive:** [ARCHITECTURE.md](./ARCHITECTURE.md) is the C4 map (system → containers → components → key signatures). This README is the orientation layer.

---

## Responsibilities

| Owns | Does not own |
|---|---|
| Multi-language extraction into a uniform node/edge/span model | DuckDB analytics / PageRank / pack assembly (`spur-analyst`) |
| Content-addressed stable symbol/file IDs | Beads / PM state (`spur-pm`) |
| Incremental rebuild + dirty-worktree overlay | TUI / orchestration (`spur-tui`, `spur-core`) |
| Staleness contract on every query response | Session cost analytics (`spur-context`) |
| Exact call/reference graph for `code_*` tools | Free-form natural-language answers |

---

## Quick start

### Build the graph for a worktree

```bash
# From a SPUR checkout (or any repo you want indexed)
scripts/spur-cargo run -p spur-cli -- graph build

# Artifact default: <worktree>/.spur/graph/
# Override: SPUR_CODE_GRAPH_INDEX=/path/to/dir  or  --output
```

### Standalone code-graph MCP server

```bash
scripts/spur-cargo run -p spur-cli -- graph mcp
# Registers GraphMcpModule: code_resolve, code_symbol_search, code_read_symbol,
# code_callers, code_callees, code_subgraph, code_file_symbols, code_symbol_info,
# code_symbol_history
```

### Programmatic pipeline (library)

```rust
use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts,
    load_artifact, resolve_worktree_root_from, write_artifact_parquet,
};

let root = resolve_worktree_root_from(std::env::current_dir()?);
let (facts, _counts) = build_facts(&root, None)?;
let artifact = artifact_from_facts(&facts, &root)?;
// write_artifact_parquet(...); load_artifact(...); artifact_from_facts_incremental(...)
```

---

## Supported languages

`Language` in `src/extract/languages.rs` (15 variants):

| Language | Grammar | Notes |
|---|---|---|
| Rust | `tree-sitter-rust` | Full tags + edge queries |
| Python | `tree-sitter-python` | + notebook fact queries |
| TypeScript / TSX / JavaScript | `tree-sitter-typescript` | JS/TSX share TSX grammar |
| C / C++ | `tree-sitter-c` / `cpp` | Inheritance + relation quality edges |
| Go | `tree-sitter-go` | |
| HCL / Terraform | `tree-sitter-hcl` | One grammar for `.hcl` and `.tf` |
| Lua | `tree-sitter-lua` | |
| Shell | `tree-sitter-bash` | |
| SQL | `tree-sitter-sequel` | |
| Markdown | `tree-sitter-md` | Sections + inline fenced code |
| Jupyter notebook | specialized | Cells / ports via `extract/notebook.rs` |

Query sources live under [`queries/`](./queries/) — see [`queries/README.md`](./queries/README.md) for the `@definition.*` capture contract and the coverage matrix.

Specialized extractors also surface **MCP tool definitions** as `NodeKind::McpTool` nodes (`extract/mcp_tools.rs`).

---

## Typed fact model

Canonical in-memory and on-disk shape: **`GraphIndexArtifact`** (`src/schema.rs`).

```text
GraphIndexArtifact
├── header / manifest_version / graph_content_hash
├── file_manifests[]     path → content_oid + node ids
├── files[] / symbols[]  stable IDs + spans
├── edges[]              calls / refs / imports / …
├── tombstones[]         deleted paths
├── diagnostics[]
└── commits[] / symbol_snapshots[] / temporal_edges[]
```

**Node kinds** (selection): Module, Function, Class, Interface, Struct, Impl, Trait, Enum, Method, Field, Constant, TypeAlias, Macro, Section, McpTool, Cell, Port, Resource, External, …

**Relation kinds:** Imports, Calls, Constructs, Contains, Implements, Defines, References, Extends, Links, Touches, Produces, Consumes, Binds, Emits.

**Edge refinement** (`GraphEdgeKind`): `Calls`, `CallsDyn`, `ReferencesHof`, `ReferencesOther`, `ReferencesAddress`.

Schema / extractor / resolver versions gate incremental validity (`SCHEMA_VERSION` and friends in `store/build.rs`). A `manifest_version` mismatch forces a full rebuild.

---

## Stable identity & content hash

These two hashes are the load-bearing walls of the crate.

### Stable symbol ID

```rust
// src/identity.rs
pub fn stable_symbol_id_for(
    relative_path: &str,
    fqn: &str,
    kind: NodeKind,
    byte_range_start: u64,
) -> String  // 16-hex u64 prefix of SHA256(path ║ fqn ║ kind_discriminator ║ le_start)
```

- Same inputs → same ID across builds and consumers (`spur-analyst` re-derives the same scheme).
- External / dependency nodes: `stable_symbol_id_for_external_path` under synthetic path `external://`.
- Newtypes: `NodeId`, `EdgeId`, `FileId`, `SpanId`, …

### Graph content hash (staleness anchor)

```rust
// src/content_hash.rs
pub fn compute_graph_content_hash(entries: /* sorted (path, oid) */) -> String
// blake3 over every indexed file’s (path, content_oid)
```

Also: `git_blob_oid` — git-compatible SHA1 blob OID (`blob <len>\0…`), tested against `git hash-object`.

---

## Module layout

The crate root (`src/lib.rs`) is a **flat re-export**: every module is `pub` and glob-re-exported. The public API is the union of module roots.

```
src/
├── lib.rs              # re-exports
├── schema.rs           # GraphIndexArtifact, NodeKind, RelationKind, load_artifact
├── identity.rs         # stable_symbol_id_for*
├── content_hash.rs     # graph_content_hash, git_blob_oid
├── discovery.rs        # .gitignore-aware file walk
├── worktree.rs         # resolve_worktree_root_from
├── git.rs / git_walk.rs
├── extract/            # tree-sitter pipeline + languages + markdown/notebook/mcp_tools
├── store/              # build, parquet, pointer, cache, commit index, lance embeddings
├── graph/              # petgraph builder + bounded subgraph algorithms
├── query_client.rs     # GraphQueryClient + Parquet / InMemory / Overlay backends
├── selector.rs         # graph://symbol/<id>, file-qualified, bare-name resolution
├── search.rs           # SearchOptions / modes / filters
├── traversal.rs        # subgraph budgets
├── temporal.rs         # TemporalIndex, symbol history, rename chains
├── validation.rs       # span anchors, code-mention validation
├── locking.rs          # flock discipline
└── mcp/                # GraphMcpModule, RebuildCoordinator, response metadata
```

### Pipeline (containers)

1. **Worktree + git** — root resolution, dirty state, history walk.
2. **Discovery** — language-grouped files.
3. **Extraction** — `build_facts` / `build_facts_for_paths` → `GraphFacts { nodes, edges, spans }`.
4. **Identity** — stamp stable IDs.
5. **Store** — `artifact_from_facts` (full) or `artifact_from_facts_incremental` → Parquet + pointer.
6. **Query** — `GraphQueryClient` trait over persisted / in-memory / overlay backends.
7. **MCP** — tool dispatch + singleflight rebuild + staleness metadata on every response.

---

## Incremental rebuild

`artifact_from_facts_incremental(prev, root)` (`store/build.rs`):

1. **Manifest gate** — version mismatch → full rebuild (`BuildMode::Full`).
2. **Discover** current tree entries.
3. **Diff OIDs** against `prev.file_manifests[*].content_oid`.
4. **Re-extract only changed paths** via `build_facts_for_paths`.
5. **Compose** buckets + tombstones → new artifact with a fresh `graph_content_hash`.

`BuildStats` reports reused vs changed vs removed paths.

### Dirty overlay (no full rebuild)

`OverlayClient<B>` composes a base backend with an in-memory delta over edited files: shadows base symbols on changed paths, remaps stable IDs that shifted on edit, and merges caller/callee edges across the base/delta boundary. Served under `RebuildCoordinator` singleflight (`mcp/rebuild_singleflight.rs`), keyed by `{ head_oid, dirty_oid_set_hash }`.

---

## Query surface

### `GraphQueryClient` trait

```rust
// src/query_client.rs (abbreviated)
pub trait GraphQueryClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult>;
    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord>;
    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord>;
    fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution>;
    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>>;
    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>>;
    fn symbols_by_files(&self, paths: &[String]) -> anyhow::Result<Vec<GraphSymbolArtifact>>;
    fn temporal_index(&self) -> Arc<TemporalIndex>;
    fn symbol_history(...) -> ...;
    // file_manifest_by_path, file_exists, …
}
```

**Backends:** `ParquetClient` · `InMemoryClient` · `OverlayClient<B>`.

### Selectors

Prefer carrying `graph://symbol/<hex-id>` once resolved. Also accepted: bare hex id, `path::name`, qualified name, bare name (may be ambiguous → candidates).

### MCP tools (`GraphMcpModule`)

| Tool | Role |
|---|---|
| `code_symbol_search` / `code_search` | Name lookup (exact/prefix/substring + filters) |
| `code_resolve` | Exact canonical name → symbol |
| `code_read_symbol` | Source body by id or path+name |
| `code_symbol_info` | Metadata only |
| `code_file_symbols` | Symbols in one file (prefer filtered search on huge files) |
| `code_callers` / `code_callees` | Impact / behavior (read `counts_by_kind` first) |
| `code_subgraph` | Bounded neighborhood map |
| `code_symbol_history` | Temporal change chain |

Registered into `spur-mcp` via `ToolModule`; also available as `spur graph mcp` and the bundled `spur mcp` server.

---

## Staleness contract

Every `code_*` response carries **`GraphResponseMetadata`**:

| Field | Meaning |
|---|---|
| `graph_content_hash` | blake3 over indexed `(path, oid)` set |
| `indexed_head_oid` | git HEAD when the artifact was built |
| `worktree_head_oid` | git HEAD at query time |
| `worktree_dirty` | tracked edits **or** HEAD ahead of indexed (untracked files do not flip this) |
| `response_file_oids_match` | tightest per-answer signal: files in *this* response byte-identical to the index |
| `rebuild_status` | `NotNeeded` \| `Fresh` \| `StaleBudgetExceeded` \| `StaleRebuildFailed` |

Prefer `response_file_oids_match` when judging trust in a single answer; use `worktree_dirty` for “should I rebuild?” decisions.

---

## On-disk layout

Resolved by `resolve_artifact_location` (`store/pointer.rs`), priority: explicit override → `CURRENT` → pointer JSON.

| Path | Role |
|---|---|
| `.spur/graph/` | Default Parquet artifact directory |
| `.spur/graph/CURRENT` | Active artifact cursor |
| `.spur/graph-index.pointer.json` | `GraphIndexPointer` (hash, manifest, indexed commit, canonical path) |
| Lance tables (feature `embed`) | Sections / code-symbol embedding sidecars |

Env override: `SPUR_CODE_GRAPH_INDEX`.

---

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `embed` | **on** | Selectable FP32/768 embedding models + LanceDB section/symbol sidecars |
| `perf-gates` | off | Compile-time performance budgets / assertions |
| `test-support` | off | Rebuild counters and test-only hooks |

`unsafe_code = "deny"`; clippy profile keeps async-lock-holding and similar safety lints active.

---

## Dependencies (high level)

| Dep | Role |
|---|---|
| `tree-sitter` + language crates | Parse + query |
| `gix` | History, refs, blobs, dirty detection |
| `blake3` / `sha2` / `sha1` | Content hash + stable IDs + git blob OIDs |
| `parquet` + Arrow | Columnar artifact I/O |
| `petgraph` | In-memory subgraph BFS |
| `fastembed` + Lance (optional) | Embedding sidecars |
| `spur-mcp` | `ToolModule` for the MCP surface |
| `ignore` | `.gitignore`-aware discovery |

### Embedding model selection

Set the model in `.spur/config.toml` (repository) or `~/.spur/config.toml` (user). Changing the model
changes the stored model lineage and automatically invalidates incompatible vectors.

```toml
[graph]
embedding_model = "nomic"
```

| Value | Model | Input contract |
|---|---|---|
| `nomic` (default), `nomic-ai/nomic-embed-text-v1.5` | Nomic Embed Text v1.5, FP32/768 | `search_query:` / `search_document:` prefixes |
| `coderank`, `nomic-coderank`, `nomic-ai/CodeRankEmbed` | CodeRankEmbed, FP32/768 | Required code-search instruction on queries; raw documents |
| `jina-code`, `jina-embeddings-v2-base-code`, `jinaai/jina-embeddings-v2-base-code` | Jina Embeddings v2 Base Code, FP32/768 | Symmetric raw query/document text |

`SPUR_EMBEDDING_MODEL` still overrides the config file when set:

```bash
SPUR_EMBEDDING_MODEL=coderank spur explore sync
```

---

## Tests & benches

- **Integration tests:** `tests/` — per-language definition/edge corpora, incremental ingest, parquet round-trip, overlay client, temporal resolution, rename corpora, MCP module, perf gates.
- **Fixtures:** `tests/fixtures/` (sample, python, typescript, cpp, markdown, rename, semantic_benchmark, …).
- **Benches:** `benches/incremental`, `benches/parquet` (criterion); `capture-baselines` binary for baseline capture.

```bash
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo bench -p spur-graph --bench incremental
```

---

## How to explore this crate

The crate is self-hosting — use the graph tools it powers (see `spurpower-code-explore`):

1. **Orient:** `knowledge_context_pack_2({ query: "stable symbol id", scope: "code" })`
2. **Read:** carry `graph://symbol/<id>` into `code_read_symbol` (modules here are often 1k–7k lines).
3. **Build path:** `code_callees` on `artifact_from_facts_incremental`.
4. **MCP router:** `code_subgraph radius=1` seeded at `GraphMcpModule::dispatch`.
5. **Ranked / path / SQL questions:** `spur-analyst` over `.spur/analyst.duckdb`, not hand-chained `code_*`.

**First files to open:** `src/lib.rs` → `schema.rs` → `identity.rs` + `content_hash.rs` → `store/build.rs` → `query_client.rs` → `mcp/mod.rs`.

---

## Related crates & docs

| Resource | Why |
|---|---|
| [ARCHITECTURE.md](./ARCHITECTURE.md) | Full C4 architecture |
| [queries/README.md](./queries/README.md) | Tree-sitter query contract + coverage matrix |
| `crates/spur-analyst` | Warm DuckDB index + evidence packs |
| `crates/spur-mcp` | MCP tool registry / transports |
| `crates/spur-cli` `graph` command | Build, vector backfill, standalone MCP |
| `docs/architecture/spur-graph-git-invalidation.md` | Content-hash invalidation design |
| `docs/superpowers/specs/*spur-graph*` | Feature design specs (notebook, HCL, parquet, …) |
