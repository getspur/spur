# Named Local-Project Query Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let user-facing SPUR MCP servers register already-indexed local Git projects and route every existing local `code_*` and analyst retrieval call to one named project through an optional `project` argument, while preserving current-project behavior and worker isolation.

**Architecture:** Put the persistent catalog, request selector, schema decoration, and MCP management module in dependency-neutral `spur-mcp`. Add opt-in catalog-aware constructors to `spur-graph` and `spur-analyst`; these resolve the name once, remove `project` before existing argument parsing, and reuse `with_worktree_root_for_request` so current artifact, rebuild, freshness, and DB-pool behavior remains authoritative. Compose a graph-plus-analyst validator and enabled modules only in user/brain surfaces (`spur-core` and standalone CLI); keep current constructors and worker catalogs project-blind.

**Tech Stack:** Rust 2021, Tokio task-local scoping, serde/TOML, `fs2` advisory file locks, atomic filesystem replacement, rmcp/JSON Schema, DuckDB, spur-graph artifacts, and existing SPUR MCP registries.

**Reference spec:** `docs/superpowers/specs/2026-07-15-external-local-project-query-design.md`

**Tracking issue:** `bd-2ztrq`

---

## Invariants to preserve

- A request without `project` must use the current worktree and keep its existing input/output shape byte-for-byte apart from normal nondeterministic metadata.
- A request with `project` may address exactly one catalog entry; there is no cross-project SQL, merge, join, or graph traversal.
- Registration validates existing graph and analyst indexes but never clones or builds anything.
- The catalog stores canonical roots only. It does not persist health or artifact paths.
- The outer request scope must enclose all existing graph rebuild/overlay and analyst freshness logic.
- Delegated workers continue to receive the current project-blind graph and analyst schemas and no catalog-management tools.
- Never trust an arbitrary path from a retrieval request. Only catalog names are accepted after registration.
- Never use bare `cargo`; all build, test, format, and lint commands go through `scripts/spur-cargo`.
- Preserve unrelated dirty-worktree files. Stage only files named by the current task.

## Expected file map

**New files:**

- `crates/spur-mcp/src/local_projects/mod.rs` — public catalog types, validator contract, routing policy, and re-exports.
- `crates/spur-mcp/src/local_projects/store.rs` — path precedence, versioned TOML, locks, permissions, atomic mutations.
- `crates/spur-mcp/src/local_projects/module.rs` — `local_project_add/list/remove` ToolModule and JSON contracts.
- `crates/spur-mcp/src/local_projects/routing.rs` — optional schema property, request extraction, scope metadata, and knowledge-pack suggestion propagation.
- `crates/spur-mcp/tests/local_projects.rs` — catalog, locking, validation, tool, and routing-helper characterization.
- `crates/spur-graph/tests/mcp_local_projects.rs` — opt-in graph schemas, routing, response scope, and concurrency.
- `crates/spur-core/src/mcp/local_projects.rs` — graph-plus-analyst registration validator and brain composition helper.
- `crates/spur-core/tests/local_project_routing.rs` — brain catalog, management, graph/analyst dispatch, and worker-isolation integration.

**Likely modified files:**

- `crates/spur-mcp/Cargo.toml`
- `crates/spur-mcp/src/lib.rs`
- `crates/spur-graph/src/mcp/mod.rs`
- `crates/spur-graph/tests/mcp_module.rs`
- `crates/spur-analyst/src/db/paths.rs`
- `crates/spur-analyst/src/mcp/mod.rs`
- `crates/spur-analyst/tests/pack/mcp_tools_characterization.rs`
- `crates/spur-analyst/tests/pack/mcp_query.rs`
- `crates/spur-core/src/mcp/mod.rs`
- `crates/spur-core/src/mcp/catalog.rs`
- `crates/spur-core/src/server/mod.rs`
- `crates/spur-core/src/server/handlers/mod.rs`
- `crates/spur-core/src/server/handlers/code_graph.rs`
- `crates/spur-core/tests/tool_catalog.rs`
- `crates/spur-core/tests/tool_schema_stability.rs`
- `crates/spur-cli/src/commands/mcp.rs`
- `docs/architecture-spur-mcp.md`
- `docs/architecture/spur-mcp-tools-architecture.md`

Keep the implementation smaller if existing helpers allow it, but do not move catalog persistence into graph, analyst, core, or CLI: that would create duplicated policy or dependency cycles.

## Execution preflight

- [ ] Record the dispatch base before editing so the final audit does not depend on a guessed commit count:

```bash
FEATURE_BASE=$(git rev-parse HEAD)
git status --short
```

Keep `FEATURE_BASE` available for Task 7. The starting worktree is intentionally dirty with unrelated user files; do not clean, stash, rewrite, or stage them.

- [ ] Read the approved spec completely, then inspect the current symbols named by each task with `knowledge_context_pack_2` and focused `code_*` calls before editing. If current code contradicts the plan, stop and record the conflict on `bd-2ztrq` rather than improvising a scope change.

---

## Task 1: Add the dependency-neutral local-project catalog substrate

**Files:**

- Create: `crates/spur-mcp/src/local_projects/mod.rs`
- Create: `crates/spur-mcp/src/local_projects/store.rs`
- Create: `crates/spur-mcp/src/local_projects/module.rs`
- Create: `crates/spur-mcp/src/local_projects/routing.rs`
- Create: `crates/spur-mcp/tests/local_projects.rs`
- Modify: `crates/spur-mcp/Cargo.toml`
- Modify: `crates/spur-mcp/src/lib.rs`

- [ ] **Step 1: Characterize the empty/default registry before adding a module**

Extend `crates/spur-mcp/tests/tool_catalog.rs` only if needed to assert the default infrastructure registry remains empty. The new catalog module must be explicit composition, not a default global tool source.

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test tool_catalog
```

Expected: PASS before and after the feature.

- [ ] **Step 2: Write failing catalog and routing-helper tests**

In `crates/spur-mcp/tests/local_projects.rs`, cover at least:

1. `SPUR_PROJECT_CATALOG` exact-path precedence, then XDG, then HOME.
2. Fresh catalog reads as version 1, generation 0, no projects.
3. Add stores a canonical absolute UTF-8 root and increments generation once.
4. Identical add is idempotent and does not increment generation.
5. Conflicting add requires `replace: true`; replace increments once.
6. Remove is idempotent; only a real removal increments generation.
7. Entries serialize in name order and unknown format versions fail closed.
8. Malformed names, relative/non-UTF-8 paths, missing roots, and validator failures return actionable typed errors.
9. A moved/missing entry remains visible from list as `unavailable` with a reason.
10. Two store instances mutating the same file do not lose updates.
11. Unix catalog directory/files have `0700`/`0600` permissions.
12. Schema decoration adds optional `project` even when `additionalProperties` is false, without changing required fields.
13. Request extraction removes `project` before domain parsing and rejects non-string/invalid names.
14. Response decoration adds top-level scope only for explicit projects.
15. Knowledge-pack `next` and `recommended_next_tools` arguments inherit the same project.

Use a fake validator implementing a dependency-neutral contract such as:

```rust
pub trait LocalProjectValidator: Send + Sync {
    fn validate(
        &self,
        requested_path: &Path,
    ) -> Result<ValidatedLocalProject, LocalProjectError>;
}

pub struct ValidatedLocalProject {
    pub canonical_root: PathBuf,
    pub health: LocalProjectHealth,
}
```

Returning the resolved worktree root is important: `local_project_add` may receive an absolute path inside a worktree, but the catalog must store the canonical Git root rather than that subdirectory.

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test local_projects
```

Expected: FAIL because the module does not exist.

- [ ] **Step 3: Implement durable catalog storage**

Add production dependencies `fs2 = "0.4"`, `toml = { workspace = true }`, and `directories = { workspace = true }` only if the final path resolver uses `directories`; avoid adding any graph/analyst dependency.

Implement these public concepts (names may vary slightly, semantics may not):

```rust
#[derive(Clone)]
pub struct LocalProjectCatalogStore { /* catalog path */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectSnapshot {
    pub generation: u64,
    pub projects: Vec<LocalProjectEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectEntry {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Clone)]
pub struct LocalProjectResolver { /* shared store */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLocalProject {
    pub name: String,
    pub root: PathBuf,
    pub catalog_generation: u64,
}
```

Implement exact location precedence from the spec. A mutation must open/create a sibling lock, acquire an exclusive `fs2::FileExt` lock, reload under lock, mutate, serialize a deterministic version-1 document, write/sync a same-directory temporary file, rename atomically, and sync the directory where supported. Read/list should take a shared lock so it cannot observe a partially replaced document.

Do not hold a process-global mutex across unrelated catalog paths. Do not persist health. Make corrupt TOML, duplicate names, and unsupported versions explicit errors rather than treating them as empty catalogs.

Default environment/XDG/HOME path discovery must be lazy: `McpCallbackServer` construction is intentionally infallible today, so a missing HOME/config directory should fail the first catalog operation with an actionable error, not panic or change unrelated server constructors. Also provide an explicit-path constructor for hermetic tests; use that instead of racing process-global environment variables. Serialize the one path-precedence test if it must alter environment state.

- [ ] **Step 4: Implement the management ToolModule**

Add `LocalProjectCatalogMcpModule<V>` (or an equivalent type-erased validator) with exactly these tool names:

```text
local_project_add
local_project_list
local_project_remove
```

`local_project_add` validates the absolute UTF-8 input, asks the injected validator to resolve the canonical Git worktree root and verify graph + analyst readiness, then mutates using the returned root. `local_project_list` reloads once and validates each stored entry live without rewriting it. `local_project_remove` changes only the catalog.

Map invalid request fields and unknown project names to JSON-RPC invalid-params errors. Map unreadable/corrupt catalog and I/O failures to internal errors with the catalog path in safe diagnostic data. Never include SQL or arbitrary file contents in errors.

- [ ] **Step 5: Implement reusable routing helpers**

The helper API must let graph and analyst opt in without coupling either crate to each other:

```rust
#[derive(Clone, Default)]
pub enum LocalProjectAccess {
    #[default]
    CurrentWorktreeOnly,
    Catalog(LocalProjectResolver),
}

pub fn with_optional_project_schema(schema: &Value) -> Value;
pub fn extract_project(
    args: &mut Value,
    access: &LocalProjectAccess,
) -> Result<Option<ResolvedLocalProject>, McpHandlerError>;
pub fn decorate_project_response(
    response: Value,
    project: Option<&ResolvedLocalProject>,
) -> Value;
```

For `CurrentWorktreeOnly`, an injected `project` must fail rather than be ignored. For `Catalog`, omitted `project` resolves to `None` and therefore current behavior. The response helper must also propagate the project name into follow-up tool argument objects without rewriting unrelated user content.

- [ ] **Step 6: Make all substrate tests green**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test local_projects
scripts/spur-cargo test -p spur-mcp --test tool_catalog
scripts/spur-cargo test -p spur-mcp
```

Expected: all PASS.

- [ ] **Step 7: Commit the substrate**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/src/lib.rs crates/spur-mcp/src/local_projects crates/spur-mcp/tests/local_projects.rs crates/spur-mcp/tests/tool_catalog.rs Cargo.lock
git commit -m "feat(spur-mcp): bd-2ztrq add local project catalog"
```

Do not stage `Cargo.lock` if dependency resolution did not change it.

---

## Task 2: Add opt-in project routing to every graph MCP tool

**Files:**

- Create: `crates/spur-graph/tests/mcp_local_projects.rs`
- Modify: `crates/spur-graph/src/mcp/mod.rs`
- Modify: `crates/spur-graph/tests/mcp_module.rs`

- [ ] **Step 1: Write failing graph contract tests**

Build two tiny indexed Git fixtures with different symbol names. Cover:

1. `GraphMcpModule::new` still advertises the exact current schemas and tool count.
2. The catalog-aware constructor advertises optional `project` on every canonical `code_*` definition and the `code_search` alias uses the same routed behavior.
3. Omitted `project` continues querying the enclosing/current worktree.
4. `project: "alpha"` reads only alpha's graph and returns the explicit scope object.
5. Unknown/unavailable names fail before graph dispatch with actionable errors.
6. Two Tokio tasks concurrently querying alpha and beta cannot leak roots.
7. Calling the project-blind constructor directly with `project` fails closed.

Run:

```bash
scripts/spur-cargo test -p spur-graph --test mcp_local_projects
```

Expected: FAIL because the catalog-aware constructor does not exist.

- [ ] **Step 2: Add opt-in module construction and schema definitions**

Change `GraphMcpModule` to carry `LocalProjectAccess` alongside `GraphMcpDeps`:

```rust
impl GraphMcpModule {
    pub fn new(deps: GraphMcpDeps) -> Self; // project-blind, unchanged callers
    pub fn with_local_projects(
        deps: GraphMcpDeps,
        resolver: LocalProjectResolver,
    ) -> Self;
}

pub fn tool_definitions() -> Vec<ToolDefinition>; // unchanged schemas
pub fn local_project_tool_definitions() -> Vec<ToolDefinition>; // decorated schemas
```

Keep aliases in registry composition as they are today; do not duplicate canonical definitions just to route an alias.

- [ ] **Step 3: Wrap existing dispatch once, outside all handlers**

Refactor the current match into a private `dispatch_current_project`. Public `dispatch` should:

1. Extract and remove `project` from args.
2. Resolve the catalog snapshot once.
3. If explicit, run the existing dispatch future inside `with_worktree_root_for_request(project.root.clone(), ...)`.
4. Decorate the successful response with scope metadata.
5. Otherwise call the existing dispatch unchanged.

Do not add project conditionals to nine individual handlers. The outer task-local scope is what preserves graph artifact lookup, dirty-worktree overlay, rebuild coalescing, and temporal history behavior.

- [ ] **Step 4: Expose a graph readiness check for registration**

Add the narrowest public helper needed by the composite validator. It should resolve the canonical graph artifact for a supplied root through `resolve_artifact_location(root, None)` and return an actionable error if absent/corrupt. Do not rebuild.

- [ ] **Step 5: Verify graph behavior**

Run:

```bash
scripts/spur-cargo test -p spur-graph --test mcp_local_projects
scripts/spur-cargo test -p spur-graph --test mcp_module
scripts/spur-cargo test -p spur-graph
```

Expected: all PASS, including unchanged default-schema snapshots.

- [ ] **Step 6: Commit graph routing**

```bash
git add crates/spur-graph/src/mcp/mod.rs crates/spur-graph/tests/mcp_local_projects.rs crates/spur-graph/tests/mcp_module.rs
git commit -m "feat(spur-graph): bd-2ztrq route code tools by project"
```

---

## Task 3: Add opt-in project routing to every analyst MCP tool

**Files:**

- Modify: `crates/spur-analyst/src/db/paths.rs`
- Modify: `crates/spur-analyst/src/mcp/mod.rs`
- Modify: `crates/spur-analyst/tests/pack/mcp_tools_characterization.rs`
- Modify: `crates/spur-analyst/tests/pack/mcp_query.rs`

- [ ] **Step 1: Write failing analyst contract tests**

Seed two temporary roots with distinct analyst DuckDB rows and graph artifacts. Cover:

1. `AnalystMcpModule::new/read_only` retain current schemas and behavior.
2. The catalog-aware constructor adds optional `project` to `query`, `doc_navigate`, `knowledge_context_pack`, and `knowledge_context_pack_2`.
3. `query` routes alpha/beta to different DBs and reports scope metadata.
4. Concurrent alpha/beta queries do not cross connection-pool entries or task-local roots.
5. `doc_navigate` and both knowledge-pack names execute under the selected root.
6. Project-scoped knowledge packs repeat `project` in `next` and `recommended_next_tools` arguments.
7. Omitted `project` snapshots remain unchanged.
8. Project-blind modules reject an injected selector.

Run:

```bash
scripts/spur-cargo test -p spur-analyst --test pack -- mcp_query
scripts/spur-cargo test -p spur-analyst --test pack -- mcp_tools_characterization
```

Expected: FAIL on the new expectations.

- [ ] **Step 2: Expose analyst DB readiness without changing selection semantics**

In `db/paths.rs`, retain `analyst_db_path()` for request-local callers. Add a public root-explicit helper that uses the same `select_analyst_db_path(root)` fallback behavior and validates the selected file can be used as the analyst DB. Registration needs a readiness/error result; it must not create or rebuild the DB.

- [ ] **Step 3: Add opt-in schemas and outer dispatch routing**

Mirror the graph construction contract:

```rust
impl AnalystMcpModule {
    pub fn new() -> Self; // project-blind
    pub fn read_only() -> Self; // project-blind
    pub fn with_local_projects(resolver: LocalProjectResolver) -> Self;
}

pub fn tool_definitions() -> Vec<ToolDefinition>; // unchanged
pub fn local_project_tool_definitions() -> Vec<ToolDefinition>; // decorated
```

Wrap all four tools at `AnalystMcpModule::dispatch`, not inside each implementation. Remove the selector before `QueryRequest` and knowledge-pack serde parsing. Scope the existing future with `spur_graph::mcp::with_worktree_root_for_request`, then decorate only explicit-project responses.

The existing `QUERY_CONNECTION_POOL` is already keyed by selected DB path; preserve that design and prove it with the concurrent test rather than adding a second pool.

- [ ] **Step 4: Verify analyst behavior**

Run:

```bash
scripts/spur-cargo test -p spur-analyst --test pack -- mcp_query
scripts/spur-cargo test -p spur-analyst --test pack -- mcp_tools_characterization
scripts/spur-cargo test -p spur-analyst
```

Expected: all PASS.

- [ ] **Step 5: Commit analyst routing**

```bash
git add crates/spur-analyst/src/db/paths.rs crates/spur-analyst/src/mcp/mod.rs crates/spur-analyst/tests/pack/mcp_query.rs crates/spur-analyst/tests/pack/mcp_tools_characterization.rs
git commit -m "feat(spur-analyst): bd-2ztrq route tools by project"
```

---

## Task 4: Compose catalog management and routing into the brain server only

**Files:**

- Create: `crates/spur-core/src/mcp/local_projects.rs`
- Create: `crates/spur-core/tests/local_project_routing.rs`
- Modify: `crates/spur-core/src/mcp/mod.rs`
- Modify: `crates/spur-core/src/mcp/catalog.rs`
- Modify: `crates/spur-core/src/server/mod.rs`
- Modify: `crates/spur-core/src/server/handlers/mod.rs`
- Modify: `crates/spur-core/src/server/handlers/code_graph.rs`
- Modify: `crates/spur-core/tests/tool_catalog.rs`
- Modify: `crates/spur-core/tests/tool_schema_stability.rs`

- [ ] **Step 1: Write failing brain/worker boundary tests**

In `local_project_routing.rs` and existing catalog tests, assert:

1. Brain `tools/list` contains all three management tools.
2. Brain graph and analyst schemas contain optional `project`.
3. Worker `tools/list` contains neither management tools nor any `project` field.
4. Brain `local_project_add/list/remove` executes through the real module with a hermetic `SPUR_PROJECT_CATALOG` path or injected store.
5. Brain dispatch routes at least one graph call and one analyst query to a registered non-current root.
6. The same omitted-project calls still route through `server.repo_root` exactly as before.
7. Registration rejects a Git root missing either graph or analyst readiness.
8. Catalog generation and explicit scope metadata agree across add and query.

Run:

```bash
scripts/spur-cargo test -p spur-core --test local_project_routing
scripts/spur-cargo test -p spur-core --test tool_catalog
scripts/spur-cargo test -p spur-core --test tool_schema_stability
```

Expected: FAIL on new tools/schemas/composition.

- [ ] **Step 2: Implement the composite validator**

`crates/spur-core/src/mcp/local_projects.rs` should implement `spur_mcp::LocalProjectValidator` by:

1. Canonicalizing the requested path, resolving its Git worktree root, and proving the result is Git-backed (do not accept `resolve_worktree_root_from`'s non-Git fallback silently).
2. Calling the graph readiness helper added in Task 2.
3. Calling the analyst readiness helper added in Task 3.
4. Returning the canonical worktree root plus `ready` only if both pass; otherwise retain a concise component-specific reason.

Expose a small constructor that creates one shared store/resolver/module set so all brain handlers read the same catalog path.

Make the composite validator and the minimal composition constructor publicly reusable from `spur_core::mcp`; `spur-cli` already depends on core and must not duplicate this policy. Keep lower-level server wiring crate-private.

- [ ] **Step 3: Register management tools as a real brain ToolModule**

Update `brain_tool_registry` to register `LocalProjectCatalogMcpModule` directly. Do not mark its names as legacy server-owned tools if the shared registry can execute the module itself. Update the catalog-only registry with a definitions-capable instance that cannot mutate during `tools/list`.

Add management definitions to the server/brain catalog only. Keep worker prelude/remainder unchanged.

- [ ] **Step 4: Route legacy brain graph/analyst handlers through enabled modules**

Store or derive a cloneable `LocalProjectResolver` in `McpCallbackServer`. Change:

- `handle_graph_tool` to use `GraphMcpModule::with_local_projects(...)`.
- `handle_analyst_tool` to use `AnalystMcpModule::with_local_projects(...)`.

Preserve the existing outer `server.repo_root` scope for omitted-project requests. An explicit project creates a nested request scope only around the domain dispatch and therefore wins for that request.

Do not change `crates/spur-core/src/worker/mcp_server.rs` or any worker registry to enabled constructors.

- [ ] **Step 5: Update exact catalog/schema expectations**

Update expected brain tool names and project-enabled schema snapshots. Add explicit negative assertions for workers so future refactors cannot accidentally expose catalog access.

- [ ] **Step 6: Verify core composition**

Run:

```bash
scripts/spur-cargo test -p spur-core --test local_project_routing
scripts/spur-cargo test -p spur-core --test tool_catalog
scripts/spur-cargo test -p spur-core --test tool_schema_stability
scripts/spur-cargo test -p spur-core --test code_graph_e2e
scripts/spur-cargo test -p spur-core --test mcp_analyst_dispatch
```

Expected: all PASS.

- [ ] **Step 7: Commit brain composition**

```bash
git add crates/spur-core/src/mcp crates/spur-core/src/server crates/spur-core/tests/local_project_routing.rs crates/spur-core/tests/tool_catalog.rs crates/spur-core/tests/tool_schema_stability.rs
git commit -m "feat(spur-core): bd-2ztrq expose local projects to brain"
```

---

## Task 5: Enable local projects in standalone MCP servers

**Files:**

- Modify: `crates/spur-cli/src/commands/mcp.rs`

- [ ] **Step 1: Write failing standalone composition tests**

Extend the inline tests in `commands/mcp.rs` (or create `crates/spur-cli/tests/mcp_local_projects.rs` if process-level setup is clearer) to assert:

1. `spur graph mcp`, `spur analyst mcp`, and bundled `spur mcp` advertise the three management tools.
2. Their local retrieval definitions contain optional `project`.
3. Bundled aliases still resolve to canonical project-aware tools.
4. An omitted selector remains scoped by `--root`.

Avoid launching an interactive stdio server in a unit test when constructing the registry and inspecting/calling it gives the same contract deterministically.

- [ ] **Step 2: Share one catalog composition helper**

Create one helper in `commands/mcp.rs` that constructs the user catalog store, resolver, composite validator, and management module. Register the management module in all three server registries, then use:

- `GraphMcpModule::with_local_projects` in graph and bundled servers.
- `AnalystMcpModule::with_local_projects` in analyst and bundled servers.

Keep `with_mcp_worktree_scope(root, ...)` around the server future so `--root` remains the default when a request omits `project`.

- [ ] **Step 3: Update server instructions**

Briefly explain in each applicable instruction string that users can register an already-indexed local project once and pass its name as `project`. Distinguish this from hosted/package `external_*` tools and state that registration does not index.

- [ ] **Step 4: Verify CLI composition**

Run:

```bash
scripts/spur-cargo test -p spur-cli commands::mcp
scripts/spur-cargo test -p spur-cli --test mcp_local_projects
```

If the integration-test file was not needed, omit the second command and note that in the worker summary.

Expected: all applicable tests PASS.

- [ ] **Step 5: Commit standalone composition**

```bash
git add crates/spur-cli/src/commands/mcp.rs crates/spur-cli/tests/mcp_local_projects.rs
git commit -m "feat(spur-cli): bd-2ztrq enable named project MCP routing"
```

Only include the integration-test path if it exists.

---

## Task 6: Document the user workflow and security boundary

**Files:**

- Modify: `docs/architecture-spur-mcp.md`
- Modify: `docs/architecture/spur-mcp-tools-architecture.md`
- Optionally modify an existing local MCP user guide if discovery identifies a better canonical page; do not edit context-service product docs as though this were an `external_*` feature.

- [ ] **Step 1: Add a concise user example**

Document the three-call flow:

```json
{"name":"notebook","path":"/Volumes/Projects/spur-notebook"}
{"query":"NotebookCell","project":"notebook"}
{"query":"SELECT file_path, symbol_name FROM symbols LIMIT 20","project":"notebook"}
```

Name the actual tools around each payload. Explain path precedence, `replace`, idempotent remove, live health, and that registration requires pre-existing graph and analyst indexes.

- [ ] **Step 2: Document routing and isolation**

Add a small architecture section showing:

```text
project name -> user catalog snapshot -> canonical root
             -> request-local worktree scope
             -> existing graph / analyst implementation
```

State explicitly that brain/user servers enable this policy, worker servers do not, and `external_*` remains the hosted package/revision surface.

- [ ] **Step 3: Check documentation references**

Run:

```bash
rg -n "local_project_(add|list|remove)|project.*code_|external_" docs/architecture-spur-mcp.md docs/architecture/spur-mcp-tools-architecture.md
git diff --check
```

Expected: examples and boundaries are discoverable; no whitespace errors.

- [ ] **Step 4: Commit documentation**

```bash
git add docs/architecture-spur-mcp.md docs/architecture/spur-mcp-tools-architecture.md
git commit -m "docs(spur-mcp): bd-2ztrq explain named local projects"
```

Add another deliberately chosen guide path only if it was actually modified.

---

## Task 7: Run cross-crate verification and harden edge cases

**Files:**

- Modify only files needed to fix failures exposed by this verification.

- [ ] **Step 1: Format all Rust changes**

Run:

```bash
scripts/spur-cargo fmt --all
git diff --check
```

Expected: formatter succeeds; no whitespace errors.

- [ ] **Step 2: Run focused affected-crate suites**

Run:

```bash
scripts/spur-cargo test -p spur-mcp
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo test -p spur-analyst
scripts/spur-cargo test -p spur-core --test tool_catalog --test tool_schema_stability --test local_project_routing --test code_graph_e2e --test mcp_analyst_dispatch
scripts/spur-cargo test -p spur-cli commands::mcp
```

Expected: all PASS.

- [ ] **Step 3: Run remote compile and lint gates**

Run:

```bash
scripts/spur-cargo check --workspace
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-mcp -p spur-graph -p spur-analyst -p spur-core -p spur-cli --all-targets -- -D warnings
```

Expected: both exit 0. A genuine remote test/compile failure is authoritative; fix it rather than retrying locally.

- [ ] **Step 4: Audit the security and compatibility diff**

Run:

```bash
rg -n 'with_local_projects|local_project_(add|list|remove)|"project"' crates/spur-{mcp,graph,analyst,core,cli}
git diff "$FEATURE_BASE"..HEAD -- crates/spur-core/src/worker crates/spur-core/src/mcp/catalog.rs
git status --short
```

Confirm:

- no worker handler uses enabled constructors;
- project-blind definitions have not gained `project`;
- every user/brain local retrieval definition has gained it;
- no request accepts an arbitrary path;
- no unrelated dirty file is staged or committed.

- [ ] **Step 5: Commit verification-only fixes if needed**

If formatting or verification required tracked changes not already committed:

```bash
git add <only the affected feature files>
git commit -m "fix(spur-mcp): bd-2ztrq harden project routing"
```

If nothing changed, do not create an empty commit.

- [ ] **Step 6: Prepare the worker completion record**

Report:

- commit OIDs and subjects;
- exact test/check/clippy commands with outcomes;
- the catalog path used by tests;
- explicit confirmation that worker schemas remain project-blind;
- any deviations from this plan and why;
- any remaining risk or follow-up that is outside the approved scope.

Do not claim completion without fresh command output from this task.
