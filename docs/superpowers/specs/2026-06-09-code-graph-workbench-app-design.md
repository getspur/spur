# Code Graph Workbench — App Gallery Design

- **Status:** Refined after AI sidebar implementation
- **Date:** 2026-06-09
- **Refined:** 2026-06-12
- **Surface:** `app_gallery/code-graph-workbench/` (a Spur App, `schema spur.app/v1`)
- **Approved visual:** notebook `Untitled105.ipynb` (dark systems-console workbench; analyst left,
  graph center, inspector bottom/right, AI conversation in the notebook sidebar)
- **Companions:** `2026-06-09-code-graph-workbench-app-integration.ipynb` (mermaid integration diagrams);
  `2026-06-09-notebook-sidebar-ai-agent-design.md` (the now-shipped AI sidebar contract)

## 1. Problem & Goal

Developers need to ask natural-language questions about the codebase ("if I change
`SpurEvent.seq`, what is the blast radius? rank by churn") and get an answer **grounded
in real graph and analyst evidence**, where every claim is traceable to a concrete
symbol node and a concrete database row.

The Code Graph Workbench is a self-contained Spur App, delivered as a Jute notebook in
`app_gallery/`. It is **sidebar-agent-driven**: the user asks the notebook AI sidebar,
which is already implemented as trusted React plus Tauri `chat_*` commands. When the
workbench app is open, the sidebar resolves the active Spur App scope, creates or resumes
an ACP session with the app's MCP server and app skill, and streams the turn through the
existing `ChatPanel`. During that turn the agent calls **app-level evidence tools** that
run deterministic queries over the SPUR code graph and `spur-analyst`
(`.spur/analyst.duckdb`), serialize Arrow, and push the results into the notebook's
reactive ports. Those pushes cascade into a graph canvas, an analyst/evidence panel, and
a symbol inspector. The same sidebar turn streams a citation-bearing answer.

**The agent chooses what to retrieve; the tools compute and push it.** The LLM only
selects arguments (which symbol / file / depth); the tools run the real SQL/graph
queries, so every painted node and row is real, never hallucinated.

### Non-goals (YAGNI, out of MVP scope)

- A workbench-owned embedded chat or `wb-answer` AI node. The sidebar is the single agent
  surface; the app contributes tools, source ports, panels, and skill.
- Tier-2 "attach to the live orchestration brain session" (see §10).
- The external-host chat model (B2): a long-lived session driving the notebook from
  outside.
- Multi-session history / persistence; graph layout engine beyond force + the cited path;
  any write actions; multi-repo. Single repo, single ACP session.

## 2. Grounding (verified against code + live `.spur/analyst.duckdb`)

This design composes existing primitives; nothing here is net-new transport.

**The sidebar agent is now the shipped primitive.** The current implementation is
`crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx` plus
`crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs`. `chat_turn` resolves
the active app scope, ensures a sidebar ACP session, streams `ChatEvent` values to trusted
React over a Tauri `Channel`, forwards permission requests, and cancels through the stored
session token. The session manager in `crates/spur-notebook/src/sidebar_chat/manager.rs`
calls `new_session(scope.cwd, scope.mcp_servers)` and `load_session(...).mcp_servers(...)`;
the scope resolver in `crates/spur-notebook/src/sidebar_chat/scope.rs` always includes the
foundation notebook MCP proxy and appends the app MCP server from `spur-app.json`.

**The app contract is shipped.** `AppScope` carries `{ cwd, mcp_servers, skill, app_key,
label }`; `resolve_app_scope` discovers `spur-app.json`, reads optional `skill/SKILL.md`
or the manifest's explicit `skill`, and turns a Python app MCP server with requirements
into `uv run --with-requirements ... python server/main.py`. Therefore the workbench
should not create its own AI DAG cell. It should provide the app artifacts the sidebar
already consumes.

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

**Analyst panels are existing views** (verified live; 59,122 symbols / 121,732 resolved
edges):

| Panel / need | Source object | Key columns |
|---|---|---|
| Blast radius / ranked impact | `v_blast_radius` | `caller_count, hot_caller_count, caller_churn_90d, self_churn_90d, blast_radius_score` |
| Inspector scorecard | `v_symbol_scorecard` | `pagerank, in_degree, out_degree, component_id, community_id, callers, importers, churn_90d, last_touched, posture, blast_radius_score` |
| Co-change ring | `v_file_cochange` | `file_a, file_b, cochange_count, has_static_edge` |
| Hotspots | `v_fix_hotspots` | `file_path, commits, fix_commits, fix_pct` |
| Subgraph + reachability | `duckpgq_nodes`, `duckpgq_edges` | `edge_kind in (calls, references_other, references_hof, calls_dyn)` |
| Co-change graph | `v_file_cochange` | `file_a, file_b, cochange_count, has_static_edge` |
| Live index counts | `_meta` | `node_count, resolved_edge_count, graph_content_hash` |

**Structural caution (verified via code graph + spur-analyst).** The original draft
depended on young AI-node plumbing. The refined integration depends instead on the
current sidebar path: `ChatPanel`, `chat_turn`, `SidebarChat`, and `resolve_app_scope`.
These are still active surface area, so **the implementation plan MUST re-verify them
against HEAD before building** (Task 0, §10).

## 3. Architecture (sidebar agent + app-level evidence tools)

```
user ──ask──▶ AI sidebar ChatPanel
                 │
                 ▼
        chat_turn(notebook_path, prompt, agent)
                 │  resolve_app_scope(app.ipynb, notebook MCP socket)
                 │  new/load ACP session with:
                 │    • foundation notebook MCP proxy
                 │    • workbench app MCP server
                 │    • workbench skill context
                 ▼
        agent subprocess during sidebar turn
                 │  calls APP-LEVEL evidence tools:
                 │    wb_blast_radius(symbol) / wb_subgraph(symbol,depth,kinds)
                 │    wb_scorecard(symbol)    / wb_cochange(file)
                 ▼
   app plugin server (server/, FastMCP, spawned from spur-app.json)
                 │  • read .spur/analyst.duckdb READ-ONLY
                 │  • optionally call foundation code_* via notebook MCP proxy
                 │  • build pyarrow Tables, serialize Arrow IPC
                 │  • notebook_push_source(port, ipc_bytes)  ─────────┐
                 ▼                                                    │
            ReactiveEngine.push_source ──cascade──▶ AFM widgets ◀─────┘
            wb-graph (center) · wb-analyst (left) · wb-inspector (bottom)
                 │
                 └─ same sidebar turn streams answer/tool chips in ChatPanel
```

The deterministic Python retrieval cell from the prior draft remains **removed**.
`wb-answer` is now also **removed**. Retrieval is the sidebar agent invoking app evidence
tools; the tools are the only writers of the evidence source ports. This is the
composition of two existing primitives (sidebar chat + `notebook_push_source`) with no new
transport.

## 4. App package & ports

```
app_gallery/code-graph-workbench/
├── spur-app.json        # schema spur.app/v1, open_mode "app",
│                        # capabilities { "active_output_scripts": true }
│                        # mcp_server { type python, entry server/main.py, requirements server/requirements.txt }
│                        # skill "skill/SKILL.md"
│                        # sdk { "typescript": "sdk" }
├── app.ipynb            # the DAG below (declares the source ports)
├── skill/
│   └── SKILL.md         # tells the sidebar agent when/how to call wb_* tools
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
├── conftest.py
├── tests/               # pytest (mirrors html_video/tests)
├── sdk/
│   ├── call_tool.ts
│   └── wire.ts
└── README.md
```

**Declared source ports** (`cell.metadata.spur.dag` / `frontend`):

| port | declared on | pushed by | bound by |
|---|---|---|---|
| `subgraph` | source port | `wb_subgraph` tool | `wb-graph` |
| `analyst_rows` | source port | `wb_blast_radius` tool | `wb-analyst` |
| `scorecard` | source port | `wb_scorecard` tool | `wb-graph`, `wb-inspector` |
| `cochange` | source port | `wb_cochange` tool | `wb-inspector` |

Frontend cells: `wb-graph`, `wb-analyst`, `wb-inspector`, and optional compact
workbench controls/status (all `anywidget-afm`). The conversation lives in the notebook
AI sidebar, not in the app DAG. No Python retrieval cell and no AI node.

## 5. App-level evidence tools (the plugin server)

Each `wb_*` tool is a typed FastMCP function (html_video pattern):

1. Resolve `.spur/analyst.duckdb` by walking up from `cwd`; open `read_only=True`.
2. Run the mapped view query from §2 (e.g. `wb_blast_radius` → `v_blast_radius` +
   `v_symbol_scorecard`; `wb_subgraph` → bounded `duckpgq_*` + a shortest path; cheaper
   graph hops may call `code_*` via `SPUR_NOTEBOOK_MCP_SOCKET`).
3. Build a `pyarrow.Table`, serialize to Arrow IPC bytes.
4. `spur_app.notebook.NotebookClient.push_source(port, ipc_bytes)` to call
   `notebook_push_source` and paint the evidence source port (queues the engine → cascade).
5. Return a small JSON result including the `stable_symbol_id`s pushed, so the agent's
   `[n1]`/`[n2]` citation markers map deterministically onto the painted nodes.

Index size is read live from `_meta` (never hardcoded). Concurrency with an indexer
rebuild is tolerated (read-only open, retry transient locks).

## 6. Sidebar agent wiring

The workbench uses the existing sidebar path:

- **Agent:** selected in the AI sidebar (`chat_agents_list` / `ChatPanel` agent selector).
- **Scope:** `resolve_app_scope(app.ipynb, socket)` sets `cwd` to the workbench app root,
  includes the foundation notebook MCP proxy, appends the workbench Python MCP server,
  and reads `skill/SKILL.md`.
- **Session:** `SidebarChat::ensure_session` creates one ACP session per `app_key`; app
  sessions are keyed by app root, while ordinary notebooks use `notebook:<path>`.
- **Tool access:** the agent sees foundation notebook tools (`code_*`,
  `notebook_push_source`) plus app `wb_*` tools through the session's `mcp_servers`.
- **Grounding contract:** the app skill instructs the agent to call the relevant `wb_*`
  evidence tool(s) **before** answering, answer only from pushed evidence, and include a
  compact citation block mapping markers to `stable_symbol_id`s returned by the tools.
- **Permissions:** handled by the sidebar's existing ACP permission forwarding; read-only
  evidence tools should not request file writes.
- **Lifecycle:** cancellation, session reuse, load-session, tool-call chips, and streaming
  answer text are owned by the sidebar.
- **Population guarantee:** if the agent skips a tool, panels keep last state and a "no
  evidence pushed this turn" status shows; the sidebar answer still renders.

## 7. Frontend panels

Three primary `anywidget-afm` widgets (sandboxed iframe) bind the source ports; layout
keeps the approved mock's workbench feel while leaving conversation to the real notebook
sidebar. The main viewport becomes analyst left, graph center, inspector bottom/right,
and status rail top. Node-select in `wb-graph` may push a selected-symbol source or call a
small app tool if the app needs deterministic follow-up; it should not implement chat.
**Scripts-off baseline (mandatory):** each widget server-renders its last port state so
the app reads as designed with active content off; live interactivity is progressive
enhancement.

## 8. Error handling, concurrency, staleness

- **Missing `.spur/analyst.duckdb`:** the evidence tool returns a guided empty state; the
  panel renders an honest stub, not fake data.
- **Stale index:** banner from `_meta.graph_content_hash` vs `git HEAD`; the workbench
  answers from the indexed snapshot and says so.
- **Tool / agent failure:** a failed evidence push leaves the other panels live; an agent
  failure surfaces in the AI sidebar; retrieval state is preserved.
- **Cancellation:** sidebar Stop / `chat_cancel` cancels the ACP turn; already-pushed
  ports stay rendered.
- **Concurrency:** read-only DuckDB open; retry transient locks; engine debounces pushes
  per source.
- **Installed app isolation:** the Deno kernel runs config-free
  (`DENO_NO_PACKAGE_JSON=1` in the bundled kernelspec), so installed apps under
  `~/.spur/apps/` are immune to ancestor `package.json` resolution breakage.

## 9. Testing

- **Evidence tools:** pytest in `tests/` (html_video pattern) against a small fixture
  `analyst.duckdb`: assert each tool's view query columns, Arrow schema, and the
  `notebook_push_source` payload (port + IPC bytes) via a stub socket.
- **Sidebar integration:** exercise `resolve_app_scope` for the workbench manifest:
  foundation MCP first, workbench MCP second, `skill/SKILL.md` loaded, and app key scoped
  to the app root.
- **Sidebar turn:** existing sidebar tests cover `chat_turn`, streaming, permissions, and
  session creation; add only workbench-specific coverage if the manifest or skill changes
  expose a regression.
- **Cascade:** integration test that a `notebook_push_source` on each evidence port reruns
  the bound widget cell (mirrors `push_source_reruns_only_downstream_cells_in_dependency_order`).
- **App mode:** smoke test that the frontend cells render in document order.
- **Doctor:** run `notebook_app_doctor` (exists) before pack.
- **Packaging:** `notebook_export_spur_app` produces a clean `.spurapp`; import preflight
  passes (deps + `mcp_server` + `skill/SKILL.md` present). Packaging acceptance depends on
  the packer-parity precondition (authored manifest + `server/` + `skill/` + `sdk/` bundled).

## 10. Plan preconditions & deferred work

- **Task 0 — API-drift check:** complete on 2026-06-13; re-verified `ChatPanel`,
  `chat_turn`, `SidebarChat`, `resolve_app_scope`, `push_source`/`SourcePush`, and
  `notebook_push_source` against HEAD. All §2 primitives are verified; `AppScope` lives in
  `crates/spur-notebook/src/sidebar_chat/types.rs`.
- **Preconditions (epic'd):** `spur_app.notebook.NotebookClient`; packer parity; doctor
  plugin-prefix gate; app package seed.
- **Task 1 — app package:** create `app_gallery/code-graph-workbench/` with
  `spur-app.json`, `app.ipynb`, `skill/SKILL.md`, and the Python FastMCP evidence server.
- **Task 2 — evidence tools:** implement and test `wb_blast_radius`, `wb_subgraph`,
  `wb_scorecard`, and `wb_cochange` as deterministic read-only tools that push Arrow IPC
  to declared ports and return citation metadata.
- **Task 3 — workbench widgets:** build graph, analyst, inspector, and status widgets
  that bind only to source ports and tolerate missing/stale evidence honestly.
- **Deferred (Tier-2):** attach the sidebar session manager to the live orchestration
  brain session ("no panel change"); the external-host chat model (B2); write actions;
  multi-repo.
