# AI Sidebar Context Provider Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-ai-sidebar-context-provider-design.md`
**Design epic:** `bd-f1ab`

**Goal:** Give the AI sidebar a pull-first queryable context substrate: one ref convention, a layered datasource catalog tool, an orientation pack tool (delivering the app skill), a DAG lineage walker, and lens/orient turn framing.

**Architecture:** All new tools are hosted by the notebook daemon's existing rmcp server (`crates/spur-notebook/src/mcp/`) and follow the established `tool()`/`call(deps, args)` module pattern. Shared logic lives in a new `crates/spur-notebook/src/context/` module. The table↔session link is derived at query time, never stored. This plan covers spec slices 1–2 only; slice 3 (spur-graph fact layer) is a follow-up epic blocked on the `2026-06-10-spur-graph-jupyter-notebook-support` plan.

**Tech Stack:** Rust, rmcp, serde_json, existing `NotebookDag`/`PortStore`/`DatasourceEntry` machinery. No new crate dependencies.

**Build/test command:** always `scripts/spur-cargo test -p spur-notebook`, never bare cargo.

---

### Task 1: Ref convention module

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/src/context/mod.rs`
- Create: `crates/spur-notebook/src/context/refs.rs`
- Modify: spur-notebook crate root (the file declaring `pub mod mcp;` / `pub mod sidebar_chat;`) — add `pub mod context;`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `Ref::parse` / `Display` round-trip for all four schemes with and without `@v<N>` anchors
- [ ] Invalid scheme / malformed version produce typed errors, not panics
- [ ] `scripts/spur-cargo test -p spur-notebook context::refs` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files + one-line module wiring
- OUT of scope: any MCP tool, sidebar_chat, jute backend types
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** (`crates/spur-notebook/src/context/refs.rs`, `#[cfg(test)] mod tests`)

```rust
#[test]
fn round_trips_all_schemes() {
    for raw in [
        "ds://polymarket",
        "ds://polymarket/markets",
        "cell://a3f1",
        "cell://a3f1@v7",
        "port://markets",
        "port://markets@v12",
        "sym://a3f1/load_df",
    ] {
        let parsed = Ref::parse(raw).expect(raw);
        assert_eq!(parsed.to_string(), raw);
    }
}

#[test]
fn rejects_unknown_scheme_and_bad_version() {
    assert!(matches!(Ref::parse("http://x"), Err(RefError::UnknownScheme(_))));
    assert!(matches!(Ref::parse("cell://a@vx"), Err(RefError::BadVersion(_))));
    assert!(matches!(Ref::parse("ds://"), Err(RefError::Empty)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-cargo test -p spur-notebook context::refs -- --nocapture`
Expected: FAIL (module/type not defined)

- [ ] **Step 3: Implement**

```rust
//! Context refs — the one reference convention from the context-provider spec §3.
//! v1 refs are notebook-relative; the notebook scope comes from the tool's
//! `notebook_path`/daemon current path.
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Datasource { id: String, table: Option<String> },
    Cell { id: String, version: Option<u64> },
    Port { name: String, version: Option<u64> },
    Symbol { cell_id: String, name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error("unknown ref scheme: {0}")]
    UnknownScheme(String),
    #[error("bad version anchor: {0}")]
    BadVersion(String),
    #[error("empty ref body")]
    Empty,
}

impl Ref {
    pub fn parse(raw: &str) -> Result<Self, RefError> {
        let (scheme, body) = raw
            .split_once("://")
            .ok_or_else(|| RefError::UnknownScheme(raw.to_owned()))?;
        if body.is_empty() {
            return Err(RefError::Empty);
        }
        let (body, version) = split_version(body)?;
        match scheme {
            "ds" => {
                let (id, table) = match body.split_once('/') {
                    Some((id, table)) => (id.to_owned(), Some(table.to_owned())),
                    None => (body.to_owned(), None),
                };
                Ok(Ref::Datasource { id, table })
            }
            "cell" => Ok(Ref::Cell { id: body.to_owned(), version }),
            "port" => Ok(Ref::Port { name: body.to_owned(), version }),
            "sym" => {
                let (cell_id, name) = body
                    .split_once('/')
                    .ok_or_else(|| RefError::UnknownScheme(raw.to_owned()))?;
                Ok(Ref::Symbol { cell_id: cell_id.to_owned(), name: name.to_owned() })
            }
            other => Err(RefError::UnknownScheme(other.to_owned())),
        }
    }
}

fn split_version(body: &str) -> Result<(&str, Option<u64>), RefError> {
    match body.rsplit_once("@v") {
        Some((head, tail)) => {
            let version = tail
                .parse::<u64>()
                .map_err(|_| RefError::BadVersion(body.to_owned()))?;
            Ok((head, Some(version)))
        }
        None => Ok((body, None)),
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Datasource { id, table: Some(table) } => write!(f, "ds://{id}/{table}"),
            Ref::Datasource { id, table: None } => write!(f, "ds://{id}"),
            Ref::Cell { id, version: Some(v) } => write!(f, "cell://{id}@v{v}"),
            Ref::Cell { id, version: None } => write!(f, "cell://{id}"),
            Ref::Port { name, version: Some(v) } => write!(f, "port://{name}@v{v}"),
            Ref::Port { name, version: None } => write!(f, "port://{name}"),
            Ref::Symbol { cell_id, name } => write!(f, "sym://{cell_id}/{name}"),
        }
    }
}
```

`context/mod.rs`: `pub mod refs;`. If `thiserror` is not already a spur-notebook dependency, hand-implement `std::error::Error` + `Display` instead of adding a dependency.

- [ ] **Step 4: Run to verify pass**

Run: `scripts/spur-cargo test -p spur-notebook context::refs -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/context/
git commit -m "feat(spur-notebook): C1.a add context ref parser and formatter"
```

---

### Task 2: Catalog tree builder with derived usage links

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-notebook/src/context/catalog.rs`
- Modify: `crates/spur-notebook/src/context/mod.rs` (add `pub mod catalog;`)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] csv/parquet/json entries become `node_type: "table"` leaves at layer 2; api_tables/duck_db/sqlite become `node_type: "connection"` with table children (spec §4 layer table)
- [ ] Datasource ids are stable slugs; name collisions disambiguated with a 6-hex hash of `(path, kind)`
- [ ] `scope=used` returns only entries referenced by `dag.source` or by literal `invoke`-name match over cell sources, with `used_by` evidence
- [ ] Serialized output contains no key matching `token|authorization|secret|password` (trust invariant §10)
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `context/catalog.rs`, `context/mod.rs`
- OUT of scope: MCP tool wiring (task-3), `DatasourceEntry` definition in jute, sidebar code
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn csv_is_a_leaf_and_api_tables_is_a_connection() {
    let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];
    let nodes = catalog_layer1(&entries);
    assert_eq!(nodes[0].node_type, "table");
    assert_eq!(nodes[1].node_type, "connection");
    assert!(nodes[1].children.len() > 0);
}

#[test]
fn colliding_names_get_hash_suffix() {
    let entries = vec![csv_entry("sales", "/a/sales.csv"), csv_entry("sales", "/b/sales.csv")];
    let ids: Vec<_> = entries.iter().map(|e| datasource_id(e, &entries)).collect();
    assert_ne!(ids[0], ids[1]);
    assert!(ids[0].starts_with("sales"));
}

#[test]
fn scope_used_matches_dag_source_and_invoke_literal() {
    // cell A declares dag.source kind "csv" port "sales"; cell B calls polymarket_markets()
    let root = notebook_with_source_and_invoke();
    let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];
    let used = used_by_map(&root, &entries);
    assert!(used.contains_key("sales"));
    assert!(used.values().flatten().any(|u| u.via == "table_function"));
}
```

Test fixtures reuse the `NotebookRoot`/`Cell` builder pattern from
`crates/spur-notebook/src/mcp/tools/notebook_dag_status.rs` tests (the
`notebook(...)`/`cell(...)` helpers) and construct `DatasourceEntry` values
directly (type defined in `jute::commands`, fields `name`, `path`, `kind`,
`group`, `columns`, `row_count`, `tables`).

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-cargo test -p spur-notebook context::catalog -- --nocapture`

- [ ] **Step 3: Implement**

Core shapes (serde Serialize, snake_case):

```rust
#[derive(Debug, serde::Serialize)]
pub struct CatalogNode {
    pub r#ref: String,                 // Ref::Datasource rendered
    pub node_type: &'static str,       // "connection" | "table"
    pub name: String,
    pub kind: String,                  // DatasourceKind as string
    pub status: &'static str,          // "connected" for api_tables, "static" otherwise (v1)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CatalogNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke: Option<String>,        // api table function call syntax, e.g. "markets()"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<UsedBy>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsedBy {
    pub cell: String,                  // cell://<id>@v<N>
    pub via: &'static str,             // "dag.source" | "table_function"
}

pub fn datasource_id(entry: &DatasourceEntry, all: &[DatasourceEntry]) -> String { /* slug + collision hash */ }
pub fn catalog_layer1(entries: &[DatasourceEntry]) -> Vec<CatalogNode> { /* node per entry, children from entry.tables */ }
pub fn descend(entries: &[DatasourceEntry], target: &Ref) -> Option<CatalogNode> { /* one level per call; table leaf carries full columns */ }
pub fn used_by_map(root: &NotebookRoot, entries: &[DatasourceEntry]) -> BTreeMap<String, Vec<UsedBy>> { /* see below */ }
```

`datasource_id`: lowercase name, non-`[a-z0-9_-]` → `-`; if another entry slugs
identically, append `-` + first 6 hex chars of a `DefaultHasher` over
`(path, kind)`.

`used_by_map`: for each code cell, (a) if `cell.metadata.spur.dag.source` is
present, attribute the entry whose `kind`/`name` matches the `DagSource.kind`/
`port` pair; (b) literal substring search of each table-leaf `invoke` name
(e.g. `"markets("`) over `cell.source` text — slice-1 mechanism per spec §4;
slice 3 upgrades to extractor facts. Cell refs are rendered with the cell's
`metadata.spur.version` anchor.

`status`: `"connected"` for `api_tables`, `"static"` for file kinds — the
catalog never exposes credentials; include a test serializing layer 1 +
descended nodes to JSON and asserting no key matches
`token|authorization|secret|password` (case-insensitive).

- [ ] **Step 4: Run to verify pass**, then **Step 5: Commit**

```bash
git add crates/spur-notebook/src/context/
git commit -m "feat(spur-notebook): C1.b catalog tree with derived usage links"
```

---

### Task 3: `notebook_catalog` MCP tool

**Task ID:** `task-3`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/notebook_catalog.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs` (add `pub mod notebook_catalog;` + `notebook_catalog::tool()` in `tools()`)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arm next to `"notebook_dag_status"` at ~line 232)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] No `ref` → layer-1 listing; `ref: "ds://<id>"` → that node + children; `ref` to a table leaf → full `columns` with `sql_type`s and `invoke`
- [ ] `scope: "used"` filters to entries with non-empty `used_by`
- [ ] Every response carries `notebook_version` and `next_queries` (deeper node refs)
- [ ] Unknown `ds://` ref → `McpError::invalid_params` whose data includes `{"code":"ref_not_found","nearest":"<layer-1 parent or null>"}` (spec §10 stale-ref handling)
- [ ] Tool registered: the `tools()` registry test in `mcp/mod.rs` includes `notebook_catalog`
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: `context/` internals (consume task-2's API as-is), `list_datasources.rs`
- If task-2's API is insufficient, emit `scope_drift` rather than reshaping `context/catalog.rs`.

**Implementation:**

- [ ] **Step 1: Failing test** — model the test harness on `notebook_dag_status.rs` tests (`TestBridge`/`TestWindows`/`deps(...)` fixture; seed `state.datasource_catalog` with two entries, load a notebook with one `dag.source` cell):

```rust
#[tokio::test]
async fn layer1_then_descend_then_used_scope() {
    let deps = deps_with_catalog_and_notebook().await;
    let layer1 = super::call(&deps, json!({})).await.unwrap();
    let body = layer1.structured_content.unwrap();
    assert!(body["nodes"].as_array().unwrap().len() == 2);
    assert!(body["notebook_version"].is_number());
    assert!(body["next_queries"].as_array().unwrap().len() > 0);

    let node = super::call(&deps, json!({"ref": body["nodes"][0]["ref"]})).await.unwrap();
    assert!(node.structured_content.unwrap()["node_type"].is_string());

    let used = super::call(&deps, json!({"scope": "used"})).await.unwrap();
    assert_eq!(used.structured_content.unwrap()["nodes"].as_array().unwrap().len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement** — follow the `list_datasources.rs` skeleton exactly; JSON schema:

```rust
pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Navigate the datasource catalog one layer at a time. Layer model: \
         catalog -> connection -> table (file kinds are leaves at layer 2). \
         Omit `ref` for layer 1; pass a `ds://` ref to descend. Use scope=used \
         to see only tables wired into this notebook. Table leaves include \
         column schemas and the invoke syntax for cells. Follow next_queries; \
         for orientation start with notebook_context_pack.",
        rmcp_object(json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string", "description": "ds:// ref to descend into; omit for layer 1" },
                "scope": { "type": "string", "enum": ["all", "used"], "default": "all" }
            },
            "additionalProperties": false
        })),
    )
}
```

`call`: read entries from `state.datasource_catalog.lock().list()`, notebook
snapshot from `state.notebook_for_path(&daemon.current_path())` (same access
pattern and error envelopes as `notebook_dag_status::call`), then delegate to
`context::catalog::{catalog_layer1, descend, used_by_map}`. `next_queries`:
one `{tool: "notebook_catalog", ref, reason}` entry per child connection or
table node returned.

- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook notebook_catalog -- --nocapture` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C1.c notebook_catalog layered tool`

---

### Task 4: Context pack assembly

**Task ID:** `task-4`

**Files:**
- Create: `crates/spur-notebook/src/context/pack.rs`
- Modify: `crates/spur-notebook/src/context/mod.rs` (add `pub mod pack;`)
- Modify: `crates/spur-notebook/src/sidebar_chat/scope.rs` (make `find_manifest_dir` and `read_skill` `pub(crate)` — no behavior change)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] Pack sections per spec §5: `app` (manifest identity + inline skill + `skill_path`; `null` for plain notebooks), `notebook` (path, cell counts, `languages` histogram from `code_type`), `catalog` (count + layer-1 summary nodes), `dag` (node/edge counts, failed/stale refs with `error_excerpt`, frontend cells with binds/emits)
- [ ] Every section stamped with `notebook_version`; list caps (12 catalog / 5 failed / 8 frontend) emit entries into `truncated[]` naming the section and the ref to query deeper
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: MCP tool wiring (task-5), `AppScope` field changes (task-9), manager.rs
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn app_section_is_null_for_plain_notebook_and_inline_for_app() {
    let plain = build_pack(&plain_fixture());
    assert!(plain["app"].is_null());
    let app = build_pack(&spur_app_fixture()); // temp dir with spur-app.json + skill/SKILL.md
    assert_eq!(app["app"]["name"], "Test App");
    assert!(app["app"]["skill"].as_str().unwrap().contains("workbench"));
}

#[test]
fn caps_emit_truncation_markers() {
    let pack = build_pack(&fixture_with_20_datasources());
    assert_eq!(pack["catalog"]["nodes"].as_array().unwrap().len(), 12);
    assert!(pack["truncated"].as_array().unwrap().iter()
        .any(|t| t["section"] == "catalog"));
}

#[test]
fn languages_histogram_counts_code_types() {
    let pack = build_pack(&fixture_py2_js1());
    assert_eq!(pack["notebook"]["languages"]["python"], 2);
    assert_eq!(pack["notebook"]["languages"]["javascript"], 1);
}
```

- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement** `pub fn build_context_pack(state: &State, notebook_path: &Path, entries: &[DatasourceEntry]) -> serde_json::Value`:
  - `app`: walk `notebook_path` ancestors via `sidebar_chat::scope::find_manifest_dir`; parse `SpurAppManifest` (same as `resolve_app_scope` does); fields `name`, `app_key` (app root path), `entry_notebook`, `open_mode`, `runtime_features`, `mcp_server: "present"|"none"` (name presence only — never command/env), `skill` (inline via `read_skill`), `skill_path`.
  - `notebook`: snapshot `(root, version)`; cell counts by `cell_kind`; `languages` histogram from `cell.metadata.spur.code_type` defaulting to `"python"` for code cells.
  - `catalog`: `context::catalog::catalog_layer1(entries)` mapped to `{ref, name, kind, node_type, status}` summaries, capped at 12.
  - `dag`: reuse `NotebookDag::from_metadata` and `PortStore::open_read_only_at` exactly as `notebook_dag_status` does; `failed` = cells whose outputs contain an `error` output (excerpt = `ename: evalue` truncated to 160 chars, same constant style as `TOOL_SUMMARY_MAX_CHARS`); `frontend_cells` from `cell.metadata.spur.frontend` (`binds`/`emits`), capped at 8.
  - `next_queries`: `notebook_catalog` (always), `notebook_lineage` for each failed ref (tool ships in task-7; emitting the name early is fine — it is a hint string, not a call).
  - `truncated`: `{section, dropped, query_deeper_via}` entries.
- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook context::pack` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C1.d context pack assembly`

---

### Task 5: `notebook_context_pack` tool + server instructions

**Task ID:** `task-5`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/notebook_context_pack.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs` (module + registry)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arm; instructions string at ~line 178)

**Depends on:** task-4

**Acceptance Criteria:**
- [ ] Tool takes no required args, returns the task-4 pack for the daemon's current notebook
- [ ] Server instructions replaced with the navigation contract (exact text below)
- [ ] Registry test includes `notebook_context_pack`; tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: `context/pack.rs` internals, sidebar code
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Failing test** — same harness as task-3; assert pack sections and `notebook_version` present in `structured_content`.
- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement** — thin shell over `context::pack::build_context_pack`; description: `"Orientation pack for the active notebook: app identity + skill, cell/language summary, datasource catalog summary, DAG health. Call this FIRST when answering from notebook state, then follow next_queries."`. Replace the instructions string in `mcp/mod.rs`:

```rust
Some("Use notebook tools to inspect and operate the active SPUR notebook. \
Navigation contract: call notebook_context_pack first to orient; every ref it \
returns (ds://, cell://, port://) is queryable — notebook_catalog descends the \
datasource tree one layer per call (catalog -> connection -> table), \
notebook_lineage walks the DAG upstream/downstream from any ref. Responses \
carry next_queries suggestions and version-anchored refs; truncated sections \
name the ref to query deeper.".into());
```

- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook notebook_context_pack` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C1.e notebook_context_pack tool and nav instructions`

---

### Task 6: Lineage walker

**Task ID:** `task-6`

**Files:**
- Create: `crates/spur-notebook/src/context/lineage.rs`
- Modify: `crates/spur-notebook/src/context/mod.rs` (add `pub mod lineage;`)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] Graph composed from: `dag.source` (datasource→cell), `NotebookDag` edges (cell→port→cell), `frontend.binds/emits` (port→frontend cell), port manifest versions, cell states
- [ ] `walk(graph, ref, direction, depth)` honors depth (default 3) and a visited cap of 100 nodes; survives a hand-corrupted cyclic metadata fixture
- [ ] Failed cells carry `error_excerpt` from their error outputs; nodes use `role: "dataset"|"job"`; edges carry `via` + `provenance: "declared"`
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two files above
- OUT of scope: MCP wiring (task-7), `NotebookDag` internals (consume its public edges)
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn upstream_walk_from_port_reaches_source_datasource() {
    // ds(csv sales) -> cell(src) -> port(raw) -> cell(viz, frontend binds raw)
    let graph = LineageGraph::build(&fixture_root(), &entries(), &port_versions());
    let out = graph.walk(&Ref::parse("port://raw").unwrap(), Direction::Upstream, 3);
    assert!(out.nodes.iter().any(|n| n.r#ref.starts_with("ds://")));
    assert!(out.edges.iter().all(|e| e.provenance == "declared"));
}

#[test]
fn depth_bound_and_cycle_cap_hold() {
    let graph = LineageGraph::build(&cyclic_metadata_fixture(), &[], &Default::default());
    let out = graph.walk(&Ref::parse("cell://a").unwrap(), Direction::Both, 50);
    assert!(out.nodes.len() <= 100);
    assert!(out.truncated);
}

#[test]
fn failed_cell_carries_error_excerpt() {
    let graph = LineageGraph::build(&fixture_with_error_output(), &[], &Default::default());
    let out = graph.walk(&Ref::parse("cell://bad").unwrap(), Direction::Both, 1);
    let job = out.nodes.iter().find(|n| n.r#ref.starts_with("cell://bad")).unwrap();
    assert_eq!(job.state, "failed");
    assert!(job.error_excerpt.as_deref().unwrap().contains("KeyError"));
}
```

The cyclic fixture writes `consumes`/`produces` metadata forming `a -> b -> a`
directly (bypassing `NotebookDag`, which would reject it) to prove the walker's
own defenses — build edges from raw metadata, not via `NotebookDag`, OR fall
back to `NotebookDag` for the happy path and raw-metadata edges only in the
cycle test. Either is acceptable; the walker must not assume acyclicity.

- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement**

```rust
pub enum Direction { Upstream, Downstream, Both }

#[derive(serde::Serialize)]
pub struct LineageNode {
    pub r#ref: String,                 // version-anchored where known
    pub role: &'static str,            // "dataset" (ds://, port://) | "job" (cell://)
    pub state: &'static str,           // "fresh" | "failed" | "unknown"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_excerpt: Option<String>, // 160-char cap
}

#[derive(serde::Serialize)]
pub struct LineageEdge { pub from: String, pub to: String, pub via: &'static str, pub provenance: &'static str }

pub struct LineageGraph { /* adjacency over String refs, node payloads */ }

impl LineageGraph {
    pub fn build(root: &NotebookRoot, entries: &[DatasourceEntry], port_versions: &BTreeMap<String, u64>) -> Self { ... }
    pub fn walk(&self, start: &Ref, direction: Direction, depth: usize) -> LineageView { ... } // BFS, visited set, cap 100
}
```

Edge derivation: per cell with `spur.dag`: `source` edge from the matching
`ds://` node (reuse `context::catalog::datasource_id`); `produces` edge cell→
`port://<name>@v<manifest version>`; `consumes` edge port→cell. Per cell with
`spur.frontend`: `binds` edge port→cell, `emits` edge cell→port. Cell state:
`"failed"` if any output is an error output, else `"fresh"` when
`execution_count.is_some()`, else `"unknown"`.

- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook context::lineage` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C2.a lineage graph and bounded walker`

---

### Task 7: `notebook_lineage` MCP tool

**Task ID:** `task-7`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/notebook_lineage.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs` (module + registry)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arm)

**Depends on:** task-6

**Acceptance Criteria:**
- [ ] Args: `ref` (required), `direction` (`upstream|downstream|both`, default `both`), `depth` (default 3)
- [ ] Topology agreement test: every `producer/consumer/port` edge reported by `notebook_dag_status` appears in a full-depth lineage walk of the same notebook
- [ ] Response includes `root`, `nodes`, `edges`, `truncated`, `next_queries`
- [ ] Ref to a deleted/unknown cell or port → `McpError::invalid_params` with `{"code":"ref_not_found","nearest":"<file-level or null>"}` (spec §10 stale-ref handling)
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: walker internals (task-6 API as-is)
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Failing tests** — task-3 harness; the agreement test calls both `notebook_dag_status::call` and `notebook_lineage::call` on the same fixture and asserts edge containment.
- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement** — schema with the three args; description: `"Walk DAG lineage from any ds://, cell://, or port:// ref. Returns dataset/job nodes with states and failed-cell error excerpts; edges carry provenance. Start from a failed/stale ref out of notebook_context_pack and walk upstream."`. `call` builds `LineageGraph::build(...)` from the current snapshot + catalog + port manifest, walks, and emits `next_queries` of `notebook_catalog` for any `ds://` leaf and `notebook_read_cell` for failed `cell://` nodes.
- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook notebook_lineage` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C2.b notebook_lineage walk tool`

---

### Task 8: Turn context payload + lens/orient preamble

**Task ID:** `task-8`

**Files:**
- Modify: `crates/spur-notebook/src/sidebar_chat/types.rs` (add `ChatTurnContext`, `NotebookViewMode`, `ChatLens`)
- Modify: `crates/spur-notebook/src/sidebar_chat/manager.rs` (`turn` signature + preamble)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs` (`chat_turn` accepts optional `context`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ChatTurnContext { notebook_path, view_mode, lens, selected_cell_ref: Option<String> }` serde round-trips camelCase (lenses spec §6 + context spec §9)
- [ ] `turn` prepends, when context is present: lens preamble line + `"Orient via notebook_context_pack before answering from notebook state."` + optional `"Focused cell: <ref>"`; prompt text unchanged when context is `None`
- [ ] Preamble differs per lens; existing manager tests still pass
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: frontend TS (lenses spec owns the UI slices), session/scope resolution, `chat_new_session`/list commands
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Failing tests** (types.rs + manager.rs test modules)

```rust
#[test]
fn chat_turn_context_round_trips_camel_case() {
    let json = r#"{"notebookPath":"/n.ipynb","viewMode":"dag","lens":"dag_ops","selectedCellRef":"cell://a3f1@v7"}"#;
    let ctx: ChatTurnContext = serde_json::from_str(json).unwrap();
    assert_eq!(ctx.lens, ChatLens::DagOps);
    assert_eq!(serde_json::to_value(&ctx).unwrap()["viewMode"], "dag");
}

#[tokio::test]
async fn turn_prepends_lens_preamble_and_orient_hint() {
    let (chat, state) = chat_with_fake();
    let ctx = ChatTurnContext { notebook_path: "/n.ipynb".into(), view_mode: NotebookViewMode::Notebook,
        lens: ChatLens::NotebookBuilder, selected_cell_ref: Some("cell://a3f1@v7".into()) };
    let (tx, _rx) = mpsc::unbounded_channel();
    chat.turn(&scope("a", "/w"), "add a chart", Some(&ctx), tx, CancellationToken::new()).await.unwrap();
    let prompt = prompt_text(&state); // first ContentBlock text of recorded PromptRequest
    assert!(prompt.starts_with("Current user perspective: Notebook builder."));
    assert!(prompt.contains("Orient via notebook_context_pack"));
    assert!(prompt.contains("Focused cell: cell://a3f1@v7"));
    assert!(prompt.ends_with("add a chart"));
}
```

- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Implement** — types per the lenses spec §3/§6 plus `selected_cell_ref`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatLens { NotebookBuilder, NotebookDeepDive, DagOps, AppProduct }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NotebookViewMode { Notebook, Dag, App }

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnContext {
    pub notebook_path: String,
    pub view_mode: NotebookViewMode,
    pub lens: ChatLens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_cell_ref: Option<String>,
}

pub fn lens_preamble(lens: ChatLens) -> &'static str {
    match lens {
        ChatLens::NotebookBuilder => "Current user perspective: Notebook builder. Help the user grow and improve this notebook; prefer concrete next cells and executable edits.",
        ChatLens::NotebookDeepDive => "Current user perspective: Notebook deep dive. Explain what the notebook does and how cells, outputs, and assumptions connect.",
        ChatLens::DagOps => "Current user perspective: DAG operations. Reason about failed, stale, and blocked nodes and recomputation order; start from the failing ref and walk lineage upstream.",
        ChatLens::AppProduct => "Current user perspective: App product. Review the rendered app as a product; suggest workflow, copy, and interaction improvements.",
    }
}
```

`turn(&self, scope, prompt, context: Option<&ChatTurnContext>, tx, cancel)`:
when `Some`, build `framed = format!("{preamble}\nOrient via notebook_context_pack before answering from notebook state.{focused}\n\n{prompt}")`
where `focused` is `"\nFocused cell: <ref>"` if present. Update the one
existing call site in `chat_commands.rs` to pass
`context.as_ref()` (new optional `context: Option<ChatTurnContext>` command
arg) and update existing manager tests to pass `None`.

- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook sidebar_chat` → PASS
- [ ] **Step 5: Commit** `feat(spur-notebook): C1.f chat turn context with lens preamble and orient hint`

---

### Task 9: Remove dead `AppScope.skill`

**Task ID:** `task-9`

**Files:**
- Modify: `crates/spur-notebook/src/sidebar_chat/types.rs` (drop `skill` field)
- Modify: `crates/spur-notebook/src/sidebar_chat/scope.rs` (stop populating it; keep `read_skill`/`find_manifest_dir` as `pub(crate)` helpers for `context::pack`; update tests)
- Modify: `crates/spur-notebook/src/sidebar_chat/manager.rs` (test fixture `scope(...)` drops the field)

**Depends on:** task-5

**Acceptance Criteria:**
- [ ] `AppScope` has no `skill` field; `resolve_app_scope` no longer reads the skill into scope (the pack tool is now the delivery path, task-4/5)
- [ ] `scope.rs` test `spur_app_dir_yields_app_scope_with_skill_and_mcp` updated to assert skill is NOT in scope (rename accordingly) while `context::pack` tests still prove inline skill delivery
- [ ] `scripts/spur-cargo test -p spur-notebook` passes workspace-wide for the crate

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above
- OUT of scope: `context/` (already consumes the helpers), chat_commands.rs, jute backend
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Adjust tests first** — change the scope.rs assertion to `assert!(...)` absence (compile fails until field removed = the failing state)
- [ ] **Step 2: Run to verify failure**
- [ ] **Step 3: Remove the field** and the `skill: read_skill(...)` initializer in `resolve_app_scope`; `default_notebook_scope` loses `skill: None`; fix the manager.rs fixture
- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-notebook` → PASS
- [ ] **Step 5: Commit** `refactor(spur-notebook): C1.g remove dead AppScope.skill (pack delivers skill)`

---

## Dependency DAG

```
task-1 ──> task-2 ──┬──> task-3
                    ├──> task-4 ──> task-5 ──> task-9
                    └──> task-6 ──> task-7
task-8 (independent root)
```

No cycles. After task-2, three branches (catalog tool, pack, lineage) run in parallel; task-8 is dispatchable immediately.

## Out of Scope (follow-up epics)

- **Slice 3** (spur-graph spur-semantic fact layer, `notebook_symbol_*` tools) — blocked on the `2026-06-10-spur-graph-jupyter-notebook-support` plan landing; plan as its own epic afterwards.
- Frontend lens UI (lenses spec slices 1–2, TypeScript).
- MCP resources mirroring, shared host crate extraction, code-intel server split (spec §8 target end state).

## Signal Expectations

All tasks: emit `scope_drift` on any out-of-scope file touch; emit `risk` if an existing public API must change shape beyond what the task describes (especially task-8's `turn` signature ripple).
