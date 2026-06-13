# MCP Crate Ownership Refactor — Design Spec

- **Status:** Draft for user review
- **Date:** 2026-06-13
- **Scope:** `crates/spur-mcp`, plus MCP-facing modules inside existing domain crates
- **Goal:** Make `spur-mcp` the shared MCP infrastructure crate, and move domain tool ownership into the crates that own the underlying capability.

## 1. Problem

`crates/spur-mcp` currently mixes two responsibilities:

1. Generic MCP infrastructure: tool definitions, `tools/list`, `tools/call`, RMCP server wiring, error/result conversion, transport/session/auth mechanics.
2. SPUR domain tools: delegation, plans, PM/beads, code graph, analyst search, worker signaling, review/reconciler operations.

This produces a god server. The central tool catalog in `crates/spur-mcp/src/tools.rs` owns every tool definition, and the central dispatcher in `crates/spur-mcp/src/server/handlers/mod.rs` routes every domain action. Adding a DuckDB MCP surface here would deepen the same coupling.

## 2. Design Principle

`spur-mcp` should provide MCP infrastructure only. Each domain crate exposes MCP modules for the capabilities it owns.

Dependency direction:

```text
domain crate -> spur-mcp infrastructure
spur-mcp infrastructure -> no domain crates
```

This means `spur-mcp` must not depend on `spur-core`, `spur-pm`, `spur-graph`, `spur-analyst`, `spur-notebook`, `spur-cost`, or product-specific crates. Those crates may depend on `spur-mcp` to publish MCP tools.

## 2.1 Current Grounding

The current tree supports the refactor diagnosis:

- `crates/spur-mcp/src/tools.rs::tools_list()` is the single brain-facing tool catalog.
- `crates/spur-mcp/src/tools.rs::worker_tools_list()` is a second curated catalog for worker MCP.
- `crates/spur-mcp/src/server/handlers/mod.rs::handle_tool_call()` is the single brain-facing dispatcher for delegation, PM, graph, analyst, plan, review, reconciler, and worker-signal tools.
- `crates/spur-mcp/src/server/mod.rs::McpCallbackServer` owns a mix of transport/session state and domain state: delegation channel, worker list, brain session binding, PM service, feature gate, active plans, reconciler outcomes, plan registry, plan ownership lock, cancellation control, continuation context, blob outcome store, reconciler task handle, startup recovery, repo root, and graph rebuild coordination.
- `crates/spur-mcp/Cargo.toml` currently depends on domain crates including `spur-analyst`, `spur-graph`, `spur-pm`, `spur-license`, `spur-blob-store`, and `spur-worktree`.
- `crates/spur-core/Cargo.toml` already depends on `spur-mcp`; moving orchestration MCP modules into `spur-core` without first moving orchestration state would otherwise preserve a circular conceptual boundary.

The refactor is therefore not just a catalog split. It must split server infrastructure from domain runtime state.

## 3. Target Crate Ownership

| Domain | Tools / Capabilities | Owner |
|---|---|---|
| PM / issue tracker | `get_issue`, `list_issues`, `update_issue`, `create_issue`, `add_dependency`, PR helper surfaces backed by PM adapter contracts | `crates/spur-pm/src/mcp/` |
| Code graph | `code_resolve`, `code_file_symbols`, `code_symbol_info`, `code_read_symbol`, `code_callers`, `code_callees`, `code_symbol_search`, `code_subgraph`, `code_symbol_history` | `crates/spur-graph/src/mcp/` |
| Analyst / DuckDB | `knowledge_context_pack`, future `duckdb_query`, `duckdb_describe`, `duckdb_explain`, `duckdb_list_tables` | `crates/spur-analyst/src/mcp/` |
| Documentation navigation | `doc_navigate`, doc-tree lookups, section reads | `crates/spur-analyst/src/mcp/` when backed by `.spur/analyst.duckdb`; otherwise `crates/spur-graph/src/mcp/` for graph-backed reads |
| Delegation / worker lifecycle | `delegate_to_worker`, `delegate_parallel`, `check_delegation_status`, `cancel_delegation`, `fetch_outcome_artifact`, `list_available_workers` | `crates/spur-core/src/mcp/` |
| Plan / review / reconciler | `submit_plan`, `execute_epic`, `merge_plan`, `resume_plan`, `force_reclaim_plan`, `get_plan_status`, `get_reconciler_status`, `get_task_diff`, `preview_task_base`, `review_task`, `submit_plan_mutation`, `plan_truncate_and_restart`, `recover_orphaned_dispatch` | `crates/spur-core/src/mcp/` |
| Worker signals | `report_signal`, `report_progress`, signal payload validation and lifecycle coordination | MCP adapter in `crates/spur-core/src/mcp/`; PM persistence through `spur-pm` |
| Notebook | Notebook state, cells, DAG, lineage, datasource catalog, app tools | remain in `crates/spur-notebook/src/mcp/`, migrated to shared `spur-mcp` infra |
| Cost | Session/project cost query tools | `crates/spur-cost/src/mcp/` when exposed |
| License | Feature/license status tools | `crates/spur-license/src/mcp/` or `crates/spur-license-admin/src/mcp/`, depending on mutation authority |

The ownership rule is: the crate that owns the business API owns the MCP adapter for that API.

Adapter ownership does not mean every policy bit moves into the domain crate. Cross-cutting MCP policy stays explicit:

- Transport/session/auth policy stays in `spur-mcp`.
- Brain/worker authority policy is chosen by the composing application.
- Plan ownership, delegation lifecycle, review authority, and worker-signal lifecycle stay with `spur-core`.
- PM persistence contracts stay with `spur-pm`, but orchestration-specific signal and audit semantics stay with `spur-core` and call into `spur-pm`.
- Shared JSON-RPC/RMCP error mapping and response shaping stay in `spur-mcp` so domain crates do not each invent protocol glue.

## 4. `spur-mcp` Infrastructure Surface

`spur-mcp` keeps only reusable MCP primitives:

- `ToolDefinition`
- `ToolRegistry`
- `ToolModule`
- `ToolCallContext`
- schema helper functions
- result/error conversion helpers
- text/JSON/table response helpers
- server builder for stdio and streamable HTTP
- optional auth/session middleware primitives
- generic `tools/list` and `tools/call` dispatch mechanics

Sketch:

```rust
pub trait ToolModule: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDefinition>;
    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: serde_json::Value,
    ) -> Result<ToolResponse, McpError>;
}

pub struct ToolCallContext<'a> {
    pub server_kind: ServerKind,
    pub authority: ToolAuthority,
    pub brain_session_id: Option<&'a spur_acp::BrainSessionId>,
    pub request_id: Option<&'a serde_json::Value>,
}

pub struct ToolRegistry {
    modules: Vec<Box<dyn ToolModule>>,
}

impl ToolRegistry {
    pub fn register<M: ToolModule>(&mut self, module: M);
    pub fn list_tools(&self) -> Vec<ToolDefinition>;
    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<ToolResponse, McpError>;
}
```

The exact trait shape can be adjusted for `rmcp` constraints, but the boundary is stable: infrastructure dispatches, domain modules implement, and per-call context carries protocol/session/authority facts that modules need without depending on `McpCallbackServer`.

`ToolRegistry` must also define duplicate-name behavior. During the compatibility phase, registering two modules with the same public tool name is an error unless the duplicate is an explicit alias for the same handler.

## 5. Composition Model

Applications compose the MCP server from domain modules.

Example for the brain/orchestrator server:

```rust
let registry = ToolRegistry::builder()
    .with(spur_core::mcp::orchestrator_module(core_deps))
    .with(spur_pm::mcp::module(pm_deps))
    .with(spur_graph::mcp::module(graph_deps))
    .with(spur_analyst::mcp::module(analyst_deps))
    .build();

spur_mcp::Server::builder()
    .name("spur-brain-mcp")
    .instructions("Use these tools to delegate work, inspect plan status, and query SPUR project state.")
    .registry(registry)
    .serve_streamable_http(addr)
    .await?;
```

Example for notebook:

```rust
let registry = ToolRegistry::builder()
    .with(spur_notebook::mcp::module(notebook_deps))
    .with(spur_analyst::mcp::duckdb_module(analyst_deps))
    .build();
```

The server identity and module set become application-level configuration, not a hard-coded `spur-mcp` catalog.

For the brain/orchestrator server, the composition site should live in the application that already owns orchestrator dependencies. `spur-mcp` must not construct `spur_core::mcp::orchestrator_module(...)` itself, because that would reintroduce the dependency direction this refactor removes.

## 6. Worker MCP

The current worker MCP is a curated subset. After the split, the subset is expressed as policy over modules/tools, not a separate copy of tool definitions.

```rust
ToolRegistry::builder()
    .with(spur_pm::mcp::module(pm_deps).allow(["get_issue", "list_issues"]))
    .with(spur_core::mcp::worker_module(core_deps))
    .with(spur_graph::mcp::module(graph_deps).read_only())
    .with(spur_analyst::mcp::module(analyst_deps).read_only())
    .build();
```

Worker authority remains narrower than brain authority. Workers can read state and emit progress/signals; they cannot self-dispatch, mutate plans directly, or claim review authority.

The policy must preserve current worker-only runtime behavior, not only the visible tool list:

- token/session validation and per-delegation context binding
- feature gates for progress and advanced PM-backed signals
- read-audit aggregation for worker read tools
- plan resolution through a `PlanResolver`-like interface
- access to reconciler outcome buffers and blob outcome materialization where required by read tools
- explicit denial of brain-only tools in both `tools/list` and `tools/call`

`allow(...)` and `read_only()` are policy builders over a module's advertised tools. They are not a substitute for runtime authority checks inside sensitive handlers.

## 7. DuckDB MCP Placement

The built-in DuckDB MCP should not be added to the current god server. It belongs in `crates/spur-analyst/src/mcp/`.

Responsibilities:

- Open `.spur/analyst.duckdb` through bundled DuckDB.
- Enforce read-only access for `analyst://current`.
- Load per-connection extensions as needed.
- Expose `duckdb_query`, `duckdb_describe`, `duckdb_explain`, and `duckdb_list_tables`.
- Share row caps, timeout policy, and SQL safety checks through `spur-analyst`, not through `spur-mcp`.

`spur-mcp` only supplies the transport and registry mechanics.

## 8. Migration Plan

### Phase 0 — Extract Shared Protocol Types and State Boundaries

Before adding module registration, separate protocol/infrastructure types from domain state:

- Move reusable `ToolDefinition`, response helpers, schema helpers, MCP/RMCP error conversion, and server transport helpers behind infrastructure modules in `spur-mcp`.
- Introduce `ToolCallContext`, `ServerKind`, and `ToolAuthority`.
- Define domain dependency structs for the current state bundles, starting with `CoreMcpDeps`, `WorkerMcpDeps`, `PmMcpDeps`, `GraphMcpDeps`, and `AnalystMcpDeps`.
- Identify which `McpCallbackServer` fields are transport/session infrastructure and which belong to `spur-core`, `spur-pm`, `spur-graph`, or `spur-analyst`.

Acceptance:

- No tool behavior changes.
- The old dispatcher can still call existing handlers.
- A code comment or test fixture documents the intended owner for each current `McpCallbackServer` domain field.
- `spur-core` is identified as the owner of active plans, plan registry, plan ownership locks, reconciler handles/outcomes, delegation lifecycle state, continuation context, and worker-signal lifecycle state.

### Phase 1 — Infrastructure Adapter Inside Existing Crate

Add `ToolRegistry` and `ToolModule` to `spur-mcp`, then make the existing `tools_list()` and `handle_tool_call()` use the registry internally. Behavior and tool names remain unchanged.

Acceptance:

- Existing MCP clients see the same tool list.
- Existing worker MCP tests pass.
- The old central dispatcher still exists but delegates to registry entries.
- The registry rejects accidental duplicate tool names.
- `tools/call` receives and forwards `ToolCallContext`; handlers no longer reach into `McpCallbackServer` for generic protocol/session facts.

### Phase 2 — PM Module Extraction

Move PM tool definitions and handlers into `crates/spur-pm/src/mcp/`.

Acceptance:

- `spur-pm` owns PM schemas and tool definitions.
- `spur-mcp` no longer imports PM tool definitions directly.
- Brain server composes the PM module.
- PM MCP handlers depend on `PmService` and PM-domain request/response types, not on `McpCallbackServer`.
- Orchestration-specific worker-signal/audit semantics do not move into `spur-pm`; they remain in `spur-core` and call PM persistence APIs.

### Phase 3 — Graph and Analyst Module Extraction

Move code graph tools into `crates/spur-graph/src/mcp/`.
Move `knowledge_context_pack`, `doc_navigate` if analyst-backed, and the new DuckDB tools into `crates/spur-analyst/src/mcp/`.

Acceptance:

- `spur-graph` owns graph selectors, graph response formats, and graph tool schemas.
- `spur-analyst` owns DuckDB and search-oriented MCP tools.
- MotherDuck MCP is no longer needed for analyst DB access.
- The code graph rebuild/singleflight policy is either moved into `spur-graph` or passed as an explicit `GraphMcpDeps` dependency; it must not remain hidden on `McpCallbackServer`.

### Phase 4 — Core Orchestration Extraction

Move delegation, plan, review, reconciler, worker-signal MCP adapters and their runtime state into `crates/spur-core/src/mcp/` or adjacent `spur-core` orchestration modules.

Acceptance:

- `spur-core` owns orchestration MCP tools.
- Worker MCP becomes a composed server with a restricted registry policy.
- `spur-mcp` has no plan/reconciler/delegation domain state.
- `spur-core` constructs the brain/orchestrator registry at the application boundary and passes it to `spur-mcp` infrastructure.
- `spur-core` no longer needs `McpCallbackServer` as a plan resolver or worker MCP dependency; it depends on explicit core-owned interfaces instead.

### Phase 5 — Notebook Adoption

Refactor `crates/spur-notebook/src/mcp/` to use the shared `spur-mcp` registry/server helpers while keeping notebook tool ownership in `spur-notebook`.

Acceptance:

- Notebook MCP retains current tool behavior.
- Notebook no longer needs a parallel custom tool catalog pattern when the shared infra is sufficient.

### Phase 6 — Dependency Cleanup and Enforcement

Remove domain dependencies from `spur-mcp`.

Acceptance:

- `spur-mcp` no longer has normal dependencies on `spur-core`, `spur-pm`, `spur-graph`, `spur-analyst`, `spur-notebook`, `spur-cost`, `spur-license`, `spur-blob-store`, or `spur-worktree`.
- Any remaining test-only dependencies are justified and do not affect the library dependency graph.
- A lightweight dependency-direction check fails if `spur-mcp` regains a domain dependency.
- The crate description and public exports describe infrastructure, not a brain callback server.

## 9. Compatibility

Tool names remain stable through the migration. Clients should not need to change from:

```text
code_read_symbol
knowledge_context_pack
get_issue
submit_plan
```

to namespaced alternatives in the first migration. Namespacing can be introduced later as aliases if needed, but the refactor should first preserve behavior.

Compatibility includes legacy aliases. For example, `code_search` must continue to route to `code_symbol_search` until a separate deprecation plan removes it.

## 10. Risks

### Circular Dependencies

The main risk is moving MCP adapters into crates that currently depend on `spur-mcp` indirectly through higher-level crates. The migration must keep `spur-mcp` at the bottom of the dependency graph.

Mitigation: domain MCP modules depend only on `spur-mcp` infra plus their crate-local domain APIs.

### Thin Registry Abstraction

A registry that only accepts `(name, args)` would force domain modules to recover session, authority, feature, and runtime dependency state through globals or server downcasts.

Mitigation: make `ToolCallContext` and typed dependency bundles part of Phase 0/1 acceptance.

### Over-Moving Orchestration State

Some current `spur-mcp` state is actually orchestrator state. Moving it into `spur-core` may expose hidden assumptions about initialization order and ownership.

Mitigation: extract registry first, then move one domain module at a time behind stable adapter traits.

### Worker Authority Regression

The worker MCP subset is security-sensitive. A registry model could accidentally expose brain-only tools to workers.

Mitigation: encode worker registry construction explicitly, with tests asserting denied tools are absent.

### Response Shape Drift

Moving handlers into domain crates can accidentally change `content` formatting, JSON-RPC error codes, or legacy aliases.

Mitigation: snapshot `tools/list`, targeted `tools/call` responses, and error cases before each extraction, then require byte-for-byte compatibility except where the phase explicitly approves a change.

## 11. Testing

Each extraction phase needs:

- Snapshot or exact-name tests for `tools/list`.
- Route tests for `tools/call`.
- Worker subset tests.
- Dependency-direction checks, either through crate graph review or a lightweight deny test.
- Existing integration tests for plans, PM, code graph, and notebook MCP behavior.
- Duplicate tool-name registration tests.
- Alias-routing tests for compatibility aliases such as `code_search`.
- Worker `tools/call` denial tests, not only `tools/list` absence tests.
- Snapshot tests for representative success and error response envelopes.

Build and test through `scripts/spur-cargo`, following the repository rule.

## 12. Definition of Done

The refactor is complete when:

- `spur-mcp` exports infrastructure only.
- `spur-mcp` has no normal dependency on domain crates.
- Existing domain crates own their MCP adapters under `src/mcp/`.
- The brain/orchestrator MCP server is composed from domain modules.
- Core orchestration state is owned by `spur-core`, not by an infrastructure server struct.
- Worker MCP authority, audit aggregation, feature gates, and denial behavior match the pre-refactor behavior.
- Notebook MCP uses shared infrastructure where practical.
- A built-in DuckDB MCP can be added through `spur-analyst/src/mcp/` without touching a central god-tool catalog.
- Existing tool names and client behavior remain compatible during migration.
