# SPUR MCP Tools Architecture

Status: Current architecture as of merge `293e616d1`

`spur-mcp` is the MCP infrastructure crate. It owns reusable transport,
registry, response, token, schema, and event primitives, but it no longer owns
SPUR domain tools.

`spur-core` owns the SPUR-specific MCP surface: delegation, plan and
reconciler operations, project-management tools, code graph tools, analyst
tools, worker-readable plan/artifact tools, and worker signals.

## Ownership Model

```mermaid
flowchart TB
    subgraph Infra["crates/spur-mcp"]
        Registry["ToolRegistry<br/>ToolModule<br/>ToolCallContext"]
        Response["JsonRpcResponse<br/>ToolResponse"]
        Transport["Streamable HTTP helpers<br/>bind / serve / shutdown"]
        Token["Worker token<br/>HMAC encode / validate"]
        EmptyCatalog["default tools_list()<br/>infrastructure-only empty catalog"]
    end

    subgraph Core["crates/spur-core"]
        CoreMcp["mcp::brain_tool_registry<br/>mcp::worker_tool_registry"]
        BrainServer["McpCallbackServer<br/>brain-facing MCP server"]
        WorkerServer["WorkerMcpServer<br/>worker-facing MCP server"]
        Orchestrator["Orchestrator<br/>injects PM, feature gate, funnel,<br/>workers, cancellation, outcome store"]
        DomainModules["Tool modules<br/>delegation / plan / catalog / signals / worker-read"]
    end

    subgraph DomainCrates["Domain providers"]
        PM["spur-pm<br/>issue and PR tools"]
        Graph["spur-graph<br/>code graph tools"]
        Analyst["spur-analyst<br/>SQL/graph analyst tools"]
        Blob["spur-blob-store<br/>outcome artifacts"]
    end

    CoreMcp --> Registry
    BrainServer --> Transport
    WorkerServer --> Token
    WorkerServer --> Registry
    Orchestrator --> BrainServer
    Orchestrator --> WorkerServer
    Orchestrator --> CoreMcp
    CoreMcp --> DomainModules
    DomainModules --> PM
    DomainModules --> Graph
    DomainModules --> Analyst
    DomainModules --> Blob
    EmptyCatalog -. compatibility .-> Registry
```

The dependency direction is intentional:

- `spur-core` composes domain tool modules into `spur_mcp::ToolRegistry`.
- `spur-mcp` does not call back into `spur-core`.
- Legacy `spur_mcp::tools_list()` and `worker_tools_list()` are empty
  infrastructure catalogs after the extraction.

## Registry Contract

Every domain-facing module implements `spur_mcp::ToolModule`:

- `tools()` returns `ToolDefinition` values for `tools/list`.
- `call(ctx, name, args)` runs the module-owned handler and returns
  `ToolResponse`.

`ToolRegistry` is a generic dispatcher:

- it rejects duplicate tool names during registration;
- it stores a module index for each tool definition;
- it resolves aliases such as `code_search -> code_symbol_search`;
- it filters or rejects denied worker calls;
- it dispatches by canonical tool name into the owning module.

The registry is deliberately unaware of SPUR semantics. SPUR-specific state
enters through dependency structs such as `DelegationMcpDeps`, `PlanMcpDeps`,
`SignalMcpDeps`, and `WorkerReadMcpDeps`.

## Brain Tool Surface

`spur-core::mcp::brain_tool_registry` composes the brain-facing catalog in this
order:

1. `DelegationMcpModule`
2. `ServerCatalogMcpModule::prelude()`
3. `PlanMcpModule::management(...)`
4. `ServerCatalogMcpModule::remainder()`
5. `PlanMcpModule::remainder(...)`
6. `SignalMcpModule`
7. alias `code_search -> code_symbol_search`

The brain server is `McpCallbackServer`. Its streamable HTTP listener is built
with `spur_mcp::server::bind_streamable_http_server`, but the service instance
is `Arc<McpCallbackServer>`.

```mermaid
sequenceDiagram
    participant Brain as Brain client
    participant HTTP as RMCP streamable HTTP
    participant Server as McpCallbackServer
    participant Registry as ToolRegistry
    participant Plan as PlanMcpModule
    participant Catalog as Server-owned handlers
    participant Module as Module-owned tools

    Brain->>HTTP: tools/call(name, arguments)
    HTTP->>Server: ServerHandler::call_tool
    Server->>Server: wait for brain_session_id
    Server->>Registry: canonical_name_for_call(name)

    alt plan tool
        Server->>Plan: call_with_server(server, ctx, name, args)
        Plan->>Server: handle_submit_plan / review_task / reconciler...
    else server-owned catalog tool
        Server->>Catalog: handle_registered_tool_call(ctx, name, args)
        Catalog->>Server: PM / code graph / analyst handler
    else module-owned tool
        Server->>Registry: call_json_tool(ctx, name, args)
        Registry->>Module: ToolModule::call(ctx, canonical, args)
    end

    Server-->>Brain: JsonRpcResponse
```

Plan tools are special. They are advertised by `PlanMcpModule`, but direct
registry invocation intentionally errors. `McpCallbackServer` rehydrates
`PlanMcpDeps` at call time and invokes `PlanMcpModule::call_with_server` so
plan ownership checks and server state remain live.

Server-owned catalog tools are also advertised through modules but dispatched by
`McpCallbackServer`:

- PM issue and PR tools from `spur-pm`;
- code graph tools from `spur-graph`;
- analyst tools from `spur-analyst`.

Delegation and signal tools are module-owned:

- `delegate_to_worker`, `delegate_parallel`, status, artifact fetch, cancel,
  and worker listing route through `DelegationMcpModule`.
- `report_signal` and `report_progress` route through `SignalMcpModule`.

Construction rule: when the orchestrator mutates server state after
`McpCallbackServer::new` with workers, cancellation control, inline wait, or
feature-backed settings, it rebuilds and installs the brain registry with
`set_tool_registry`. This matters because module dependency structs capture
some values from the server at composition time.

## Worker Tool Surface

Worker MCP is a separate, per-brain-session server. It is not the brain callback
server.

`WorkerMcpFetcher` lazy-starts and caches a `WorkerMcpServer` for each
`BrainSessionId`. For each delegation it mints a one-hour HMAC token containing:

- delegation id;
- brain session id;
- expiry timestamp.

Workers receive the worker MCP URL plus token when `enable_worker_mcp` is
enabled for a delegation.

```mermaid
sequenceDiagram
    participant Dispatch as Delegation dispatch
    participant Fetcher as WorkerMcpFetcher
    participant WServer as WorkerMcpServer
    participant Worker as Worker agent
    participant Auth as worker_auth_middleware
    participant Handler as WorkerToolHandler
    participant Sink as Worker read/signal sinks

    Dispatch->>Fetcher: fetch_url_token(brain_session, delegation_id)
    Fetcher->>WServer: ensure server for brain session
    WServer-->>Fetcher: url + issue_token(delegation_id)
    Fetcher-->>Dispatch: http://127.0.0.1:PORT/mcp?token=...
    Dispatch-->>Worker: worker MCP endpoint

    Worker->>Auth: tools/call with token or MCP session id
    Auth->>Auth: validate HMAC token and brain_session_id
    Auth->>Handler: attach WorkerCallContext
    Handler->>Handler: registry canonicalization and denial checks
    Handler->>Handler: invoke_with_lifecycle
    Handler->>Sink: get_plan_status / get_task_diff / report_signal / ...
    Sink-->>Worker: structured result
```

The worker registry is used for discovery, alias canonicalization, and denied
tool enforcement. Live worker calls are handled by RMCP-generated typed methods
on `WorkerToolHandler`, then wrapped by `invoke_with_lifecycle`, which:

- reconstructs `WorkerCallContext` from authenticated request/session state;
- rejects brain-session mismatches;
- tracks active calls and peak concurrency;
- appends read-audit entries when applicable;
- records per-delegation call latency and error status.

Worker-readable tools include:

- `get_issue` and `list_issues`;
- `get_plan_status` and `get_task_diff`;
- `fetch_outcome_artifact`;
- code graph and analyst read tools;
- `report_signal` and `report_progress`.

Worker-denied tools include brain-only delegation, plan mutation, PM mutation,
and issue-graph mutation tools such as `delegate_to_worker`, `submit_plan`,
`review_task`, `update_issue`, `create_issue`, and `graph_plan`.

## Transport Boundaries

```mermaid
flowchart LR
    BrainClient["Brain client"] --> BrainHTTP["McpCallbackServer<br/>streamable HTTP via spur-mcp helpers"]
    BrainHTTP --> BrainRegistry["Brain ToolRegistry"]
    BrainRegistry --> BrainDomains["Delegation / Plan / PM / Graph / Analyst / Signals"]

    WorkerAgent["Worker agent"] --> WorkerHTTP["WorkerMcpServer<br/>local /mcp route with auth middleware"]
    WorkerHTTP --> WorkerRegistry["Worker ToolRegistry<br/>canonicalize + deny"]
    WorkerHTTP --> WorkerRouter["RMCP ToolRouter<br/>typed worker methods"]
    WorkerRouter --> WorkerDomains["Worker read tools / signals / progress"]

    BrainDomains --> PMDB["PmService / Beads"]
    WorkerDomains --> PMDB
    BrainDomains --> Outcome["Outcome store"]
    WorkerDomains --> Outcome
```

Brain and worker MCP are separate surfaces because they have different trust
models:

- the brain surface exposes orchestration and mutation tools;
- the worker surface exposes scoped read tools and worker reporting tools;
- worker authorization is token-bound to a delegation and brain session;
- worker sessions can reuse `Mcp-Session-Id` only after the middleware has
  associated that session with a token-derived delegation context.

## Practical Extension Points

Add a new brain tool by choosing the right ownership boundary:

- Add it to an existing `ToolModule` when the tool is module-owned.
- Add only its `ToolDefinition` to a catalog module when `McpCallbackServer`
  must dispatch it directly.
- Add worker access separately in `worker_tool_registry` and
  `WorkerToolHandler`; do not assume brain catalog membership grants worker
  access.

When adding worker tools, decide three things explicitly:

- whether the tool is listed in the worker registry;
- whether it is denied by `WORKER_DENIED_TOOL_CALLS`;
- whether the live handler needs authenticated `WorkerCallContext`,
  read-audit recording, or signal/progress plumbing.
