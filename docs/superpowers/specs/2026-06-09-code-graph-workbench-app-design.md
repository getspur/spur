# Code Graph Workbench — App Gallery Design

- **Status:** Approved (brainstorming complete)
- **Date:** 2026-06-09
- **Surface:** `app_gallery/code-graph-workbench/` (a Spur App, `schema spur.app/v1`)
- **Approved visual:** notebook `Untitled105.ipynb` (dark systems-console workbench; analyst left, graph center, conversation right, inspector bottom)
- **Companion:** `2026-06-09-code-graph-workbench-app-integration.ipynb` (mermaid integration diagrams)

## 1. Problem & Goal

Developers need to ask natural-language questions about the codebase ("if I change
`SpurEvent.seq`, what is the blast radius? rank by churn") and get an answer **grounded
in real graph and analyst evidence**, where every claim is traceable to a concrete
symbol node and a concrete database row.

The Code Graph Workbench is a self-contained Spur App, delivered as a Jute notebook in
`app_gallery/`. It is **agent-driven**: the user asks the app-owned ACP agent; during its
turn the agent calls **app-level evidence tools** that run deterministic queries over the
SPUR code graph and `spur-analyst` (`.spur/analyst.duckdb`), serialize Arrow, and push
the results into the notebook's reactive ports. Those pushes cascade into a graph canvas,
an analyst/evidence panel, and a symbol inspector. The same agent turn streams a
citation-bearing answer.

**The agent chooses what to retrieve; the tools compute and push it.** The LLM only
selects arguments (which symbol / file / depth); the tools run the real SQL/graph
queries, so every painted node and row is real, never hallucinated.

### Non-goals (YAGNI, out of MVP scope)

- Tier-2 "attach to the live orchestration brain session" (see §10). The MVP agent is
  app-owned.
- The external-host chat model (B2): a long-lived session driving the notebook from
  outside. The MVP uses the in-DAG AI node (B1).
- Multi-session history / persistence; graph layout engine beyond force + the cited path;
  any write actions; multi-repo. Single repo, single ACP session.

## 2. Grounding (verified against code + live `.spur/analyst.duckdb`)

This design composes existing primitives; nothing here is net-new transport.

**The agent turn is a shipped primitive.** `crates/spur-notebook/src/dag/ai/acp_backend.rs::AcpAgentBackend`
is "Tier-1: one ACP session per notebook, one prompt turn per run": `initialize →
new_session(cwd) → prompt(PromptRequest) → stream AgentMessageChunk`, honoring a
`CancellationToken`. The AI node contract (`dag/ai/mod.rs`): `AiRunRequest { cell_id,
prompt, context: Vec<PortContext>, cancel } → AiRunOutput { text, usage }`. The header
notes "Tier-2 (session/Orchestrator) becomes a second impl with no engine change."

**The port-push cascade is a shipped primitive.** `crates/spur-notebook/src/dag/engine.rs`:
`ReactiveEngine::push_source(SourcePush { source: DagSource, payload:
SourcePayload::IpcBytes(Vec<u8>) })` queues the engine and reruns downstream cells in
dependency order. Frontends reach it via `SourcePushIntentMsg { port, payload }`; **any
MCP client reaches the identical path** via the foundation tool:

```
notebook_push_source(port: string, payload: u8[])
  // "Push Arrow IPC bytes into a declared notebook source port and queue the reactive engine."
```

`resolve_source_for_port` rejects undeclared/ambiguous ports, so every pushed port MUST
be a **declared source port**. Pushes are debounced per source with a max-in-flight cap,
so the four evidence pushes per turn are within contract.

**The app-level plugin server is a shipped model.** `app_gallery/html_video/` is the
reference: `spur-app.json` declares an `mcp_server` (`{type: "python", entry:
"server/main.py", requirements: "server/requirements.txt"}`); `server/main.py` builds a
`FastMCP` and registers plain typed Python functions as tools; logic lives in helper
modules. The foundation spawns the plugin as a stdio MCP child, injects `SPUR_PORTS_ROOT`
and `SPUR_NOTEBOOK_MCP_SOCKET` at spawn, and routes plugin tools additively (foundation
tools win on name collision). **The foundation stays app-agnostic; all workbench logic
lives in the app.**

**Real `AgentConnection` transport.** `crates/spur-acp/src/connection/native.rs::NativeAcpConnection::new(agent_name,
command, extra_args, permission_tx)` spawns the agent as a child process group, registers
the pgid under `.spur/pgids/`, and `Drop` kills it. Broadcast channel capacity 4096 is
the sizing invariant (anchor `3ff4e86`), inherited untouched. Only `NativeAcpConnection`
is in scope (stdio / cli-wrap / stream-json adapters are out).

**Analyst panels are existing views** (verified live; ≈ 52,844 symbols / 110,291 resolved
edges):

| Panel / need | Source object | Key columns |
|---|---|---|
| Blast radius / ranked impact | `v_blast_radius` | `caller_count, hot_caller_count, caller_churn_90d, self_churn_90d, blast_radius_score` |
| Inspector scorecard | `v_symbol_scorecard` | `pagerank, in_degree, out_degree, component_id, community_id, callers, importers, churn_90d, last_touched, posture, blast_radius_score` |
| Co-change ring | `v_file_cochange` | `file_a, file_b, cochange_count, has_static_edge` |
| Hotspots | `v_fix_hotspots` | `file_path, commits, fix_commits, fix_pct` |
| Subgraph + reachability | `duckpgq_nodes`, `duckpgq_edges` | `edge_kind in (calls, references_other, references_hof, calls_dyn)` |
| Co-change graph | `onager_edges` | temporal / co-change edges |
| Live index counts | `_meta` | `node_count, resolved_edge_count, graph_content_hash` |

**Structural caution (verified via spur-analyst).** `AcpAgentBackend`, `AiNodeBackend`,
`push_source`, and `SourcePush` are young **leaf** symbols (pagerank ≈ 0, callers ≤ 1,
churn ~1) under active development (the AFM reactive control-plane plan,
`docs/superpowers/plans/2026-06-05-jute-app-afm-reactive-control-plane.md`, is in flight;
the graph index advanced during design). **The implementation plan MUST re-verify these
APIs against HEAD before building** (Task 0, §10).

## 3. Architecture (B1: agent-driven, app-level evidence tools)

```
user ──ask──▶ wb-question (source port)
                 │
                 ▼
            wb-answer  [AI node = AcpAgentBackend turn over NativeAcpConnection]
                 │  during the turn the agent calls APP-LEVEL evidence tools:
                 │    wb_blast_radius(symbol) / wb_subgraph(symbol,depth,kinds)
                 │    wb_scorecard(symbol)    / wb_cochange(file)
                 ▼
   app plugin server (server/, FastMCP, spawned by foundation)
                 │  • read .spur/analyst.duckdb READ-ONLY + code_* via SPUR_NOTEBOOK_MCP_SOCKET
                 │  • build pyarrow Tables, serialize Arrow IPC
                 │  • notebook_push_source(port, ipc_bytes)  ─────────┐
                 ▼                                                    │
            ReactiveEngine.push_source ──cascade──▶ AFM widgets ◀─────┘
            wb-graph (center) · wb-analyst (left) · wb-inspector (bottom)
                 │
                 └─ same agent turn streams answer text ─▶ answer port ─▶ conversation (right)
```

The deterministic Python retrieval cell from the prior draft is **removed**. Retrieval is
now the agent invoking app evidence tools; the tools are the only writers of the evidence
source ports. This is the composition of two existing primitives (AI node + push_source)
with no new transport.

## 4. App package & ports

```
app_gallery/code-graph-workbench/
├── spur-app.json        # schema spur.app/v1, open_mode "app",
│                        # runtime.features [frontend-cells, anywidget-afm, mcp-tools, ports-arrow],
│                        # mcp_server { type python, entry server/main.py, requirements server/requirements.txt }
├── app.ipynb            # the DAG below (declares the source ports)
├── server/              # app-level MCP plugin (mirrors html_video/server)
│   ├── main.py          # FastMCP("code-graph-workbench"); register wb_* tools; stdio
│   ├── requirements.txt # mcp, duckdb, pyarrow
│   ├── analyst.py       # read-only .spur/analyst.duckdb; view queries -> pyarrow
│   ├── graph.py         # code_*/duckpgq subgraph -> pyarrow
│   ├── ports.py         # arrow IPC -> notebook_push_source over SPUR_NOTEBOOK_MCP_SOCKET
│   └── tools/
│       ├── blast_radius.py   # wb_blast_radius(symbol)
│       ├── subgraph.py       # wb_subgraph(symbol, depth, edge_kinds)
│       ├── scorecard.py      # wb_scorecard(symbol)
│       └── cochange.py       # wb_cochange(file)
├── README.md
└── tests/               # pytest (mirrors html_video/tests)
```

**Declared source ports** (`cell.metadata.spur.dag` / `frontend`):

| port | declared on | pushed by | bound by |
|---|---|---|---|
| `question` | `wb-question` | composer (frontend) | `wb-answer` |
| `subgraph` | source port | `wb_subgraph` tool | `wb-graph` |
| `analyst_rows` | source port | `wb_blast_radius` tool | `wb-analyst` |
| `scorecard` | source port | `wb_scorecard` tool | `wb-graph`, `wb-inspector` |
| `cochange` | source port | `wb_cochange` tool | `wb-inspector` |
| `answer` | `wb-answer` (AI node) | AI node output | conversation (right) |

Frontend cells: `wb-question`, `wb-graph`, `wb-analyst`, `wb-inspector` (all
`anywidget-afm`). DAG cell: `wb-answer` (AI node). No Python retrieval cell.

## 5. App-level evidence tools (the plugin server)

Each `wb_*` tool is a typed FastMCP function (html_video pattern):

1. Resolve `.spur/analyst.duckdb` by walking up from `cwd`; open `read_only=True`.
2. Run the mapped view query from §2 (e.g. `wb_blast_radius` → `v_blast_radius` +
   `v_symbol_scorecard`; `wb_subgraph` → bounded `duckpgq_*` + a shortest path; cheaper
   graph hops may call `code_*` via `SPUR_NOTEBOOK_MCP_SOCKET`).
3. Build a `pyarrow.Table`, serialize to Arrow IPC bytes.
4. `notebook_push_source(port, ipc_bytes)` over `SPUR_NOTEBOOK_MCP_SOCKET` to paint the
   evidence source port (queues the engine → cascade).
5. Return a small JSON result including the `stable_symbol_id`s pushed, so the agent's
   `[n1]`/`[n2]` citation markers map deterministically onto the painted nodes.

Index size is read live from `_meta` (never hardcoded). Concurrency with an indexer
rebuild is tolerated (read-only open, retry transient locks).

## 6. AI node + agent wiring (`wb-answer`)

`AcpAgentBackend` over a fresh `NativeAcpConnection`:

- **Agent:** the user's configured default SPUR agent (`agent_name`/`command` from SPUR
  config), `set_repo_root(repo_root)`, `cwd` = repo root.
- **Agent tool access (the one real build item):** the spawned agent's MCP config MUST
  include the **notebook MCP server** (`SPUR_NOTEBOOK_MCP_SOCKET`), which aggregates the
  foundation tools (`code_*`, `notebook_push_source`) **and** the app plugin's `wb_*`
  tools (proxied additively). Without this the agent can talk but cannot paint. This is
  Task 1 of the plan.
- **Grounding contract:** the prompt instructs the agent to call the relevant `wb_*`
  evidence tool(s) **before** answering, and to answer only from the pushed evidence,
  emitting a citation block mapping each marker to a `stable_symbol_id` returned by the
  tools.
- **Permissions:** read-only policy via `permission_tx` (auto-approve graph/analyst
  reads + `notebook_push_source`, deny writes).
- **Lifecycle:** one ACP session per notebook (`ensure_session` reuse); `CancellationToken`
  wired to a Stop control; `Drop` + `.spur/pgids/` handle teardown; broadcast 4096
  inherited.
- **Population guarantee:** if the agent skips a tool, panels keep last state and a "no
  evidence pushed this turn" status shows; the answer still renders.

## 7. Frontend panels

Four `anywidget-afm` widgets (sandboxed iframe) binding the source ports; layout matches
the approved mock (analyst left, graph center, conversation right, inspector bottom,
status rail top). `wb-question` and node-select in `wb-graph` use
`experimental.invoke("source.push", {port, payload})`. **Scripts-off baseline
(mandatory):** each widget server-renders its last port state so the app reads as designed
with active content off; live interactivity is progressive enhancement.

## 8. Error handling, concurrency, staleness

- **Missing `.spur/analyst.duckdb`:** the evidence tool returns a guided empty state; the
  panel renders an honest stub, not fake data.
- **Stale index:** banner from `_meta.graph_content_hash` vs `git HEAD`; the workbench
  answers from the indexed snapshot and says so.
- **Tool / agent failure:** a failed evidence push leaves the other panels live; an agent
  failure surfaces only in the answer column; retrieval is unaffected.
- **Cancellation:** Stop cancels the ACP turn (`conn.cancel(session_id)`); already-pushed
  ports stay rendered.
- **Concurrency:** read-only DuckDB open; retry transient locks; engine debounces pushes
  per source.

## 9. Testing

- **Evidence tools:** pytest in `tests/` (html_video pattern) against a small fixture
  `analyst.duckdb`: assert each tool's view query columns, Arrow schema, and the
  `notebook_push_source` payload (port + IPC bytes) via a stub socket.
- **AI node:** reuse the existing `FakeConn` harness in `acp_backend.rs` to assert prompt
  assembly, streaming accumulation, cancellation.
- **Cascade:** integration test that a `notebook_push_source` on each evidence port reruns
  the bound widget cell (mirrors `push_source_reruns_only_downstream_cells_in_dependency_order`).
- **App mode:** smoke test that the frontend cells render in document order.
- **Packaging:** `notebook_export_spur_app` produces a clean `.spurapp`; import preflight
  passes (deps + `mcp_server` present).

## 10. Plan preconditions & deferred work

- **Task 0 — API-drift check:** re-verify `AcpAgentBackend`, `AiNodeBackend`,
  `push_source`/`SourcePush`, and `notebook_push_source` against HEAD before building
  (these are young, actively-churning leaves; the AFM control-plane plan is in flight).
- **Task 1 — agent MCP wiring:** ensure the `NativeAcpConnection`-spawned agent is
  configured with the notebook MCP socket (foundation + app plugin tools). This is the
  critical-path integration glue.
- **Deferred (Tier-2):** attach the AI node to the live orchestration brain session (a
  backend swap behind `AiNodeBackend`, "no engine change"); the external-host chat model
  (B2); write actions; multi-repo.
