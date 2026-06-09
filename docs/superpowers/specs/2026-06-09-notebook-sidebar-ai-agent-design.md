# Notebook Sidebar AI Agent — Design

- **Status:** Approved (brainstorming complete)
- **Date:** 2026-06-09
- **Surface:** `crates/spur-notebook/jute-notebook` — a new sidebar panel + a streaming Tauri command + a Rust per-app session manager
- **Companion:** `2026-06-09-notebook-sidebar-ai-agent-integration.ipynb` (mermaid diagrams)
- **Related:** `2026-06-09-code-graph-workbench-app-design.md` (one app the sidebar agent drives)

## 1. Problem & Goal

The notebook should have a first-class **AI Agent chat in the sidebar**. Opening it gives
the user a conversational agent that is **app-aware**: by default it is scoped to the
"notebook app" (general notebook authoring), and when the user opens or switches to a
Spur App it **re-scopes its context** to that app — the app's MCP tools, working
directory, and skill. Apps integrate with the one sidebar agent by contributing tools +
skill; the agent drives the app (e.g. paints the Code Graph Workbench panels via
`notebook_push_source`) and streams a grounded answer.

This subsumes per-app embedded agents (e.g. the workbench's `wb-answer` AI node): the
**agent lives once in the sidebar**, and an app provides *tools + panels + skill*.

### Design stance: reuse, do not reinvent

Every major concern maps to shipped infrastructure. Net-new surface is small (one panel,
one streaming command, one session manager, one context loader). See §2.

### Non-goals (YAGNI)

- A second agent transport. Only `NativeAcpConnection` (the agent the user already runs).
- Replacing the existing inbound MCP agent bridge (`src/agent/`); that is orthogonal
  (external agent → notebook). This is the *outbound* user → agent path.
- Multi-window agent sharing; cross-repo sessions. One window, repo-rooted.

## 2. Grounding (verified against code)

| Concern | Decision | Reused infrastructure (file) |
|---|---|---|
| Panel | AI Agent chat panel (trusted React) | `SidebarPanel` + `SIDEBAR_PANELS` (`ui/notebook/sidebar/panels.ts`), `useSidebar.activatePanel` (`stores/sidebar.ts`) |
| Session lifecycle | **B** — per-app persistent sessions | `AgentConnection::{new_session, load_session, list_sessions}` (`spur-acp/src/connection/mod.rs`); `SpurAgentCaps::supports_load_session` (`spur_agent_caps.rs`) |
| Session discovery | cwd-scoped list, merged by id | `Orchestrator::list_sessions_from_disk` + `list_sessions_from_rpc(_with_cwd)` (`spur-core/src/orchestrator/connection.rs`) |
| Re-scope / hot-swap tools | **C as refresh** = `new_session(cwd, mcp_servers)` | `new_session` binds MCP at session-creation time (ACP spec; doc comment in `connection/mod.rs:87`) |
| Session UX | mirror the TUI picker | `SessionPickerView` (`spur-tui/src/views/session_picker.rs`) |
| Turn engine | `AcpAgentBackend` pattern | `dag/ai/acp_backend.rs` (`new_session(cwd, Vec::new()) → prompt → stream AgentMessageChunk`) |
| Streaming to UI | `chat_turn` Tauri command + `Channel<ChatEvent>` | the `run_cell` `Channel<RunCellEvent>` pattern; `prompt → Stream<SessionNotification>`; `subscribe_session_notifications` broadcast |
| Permissions | **identical to the TUI** | `permission_tx` (`NativeAcpConnection::new`), `PermissionRequest` (`spur-acp/src/types.rs`), `App::handle_permission_request`/`process_permission` + `App.pending_permission` + `SessionDetailView::{push_permission, resolve_pending_permissions}`; bypass via `AgentConfig.skip_permissions[_args|_session_mode]` + `new_session_with_bypass` (`spur-core/src/skip_perm.rs`) |
| App scope | notebook path → `spur-app.json` `mcp_server` + skill | spur-app manifest (`spur_app.rs`); html_video plugin model |
| App painting | agent tools push ports | `notebook_push_source(port, ipc_bytes)` → `ReactiveEngine::push_source` cascade |

**Structural caution (spur-analyst):** the ACP session methods (`new_session`,
`load_session`, `prompt`) and `AcpAgentBackend` are low-churn leaf seams (one caller
each); `new_session_with_bypass` is a 12-caller load-bearing wall (stable). The frontend
`useSidebar` is recently added (active). The plan must re-verify these against HEAD before
building (Task 0).

## 3. Architecture

```
┌ Sidebar (trusted React) ───────────────┐
│ ChatPanel.tsx  (registry entry "agent") │
│  • message list + streaming             │
│  • inline permission grant/deny         │
│  • session picker (per-app)             │
│  • app-scope indicator                  │
└───────────┬─────────────────────────────┘
            │ invoke("chat_turn", { sessionRef, prompt, onEvent: Channel<ChatEvent> })
            │ invoke("chat_sessions_list" / "chat_switch_session" / ...)
            ▼
┌ Tauri backend (src-tauri/commands) ─────┐
│ SidebarChat session manager (Rust)      │
│  • holds AgentConnection (NativeAcp)     │
│  • per-app session map (by cwd+app)      │
│  • new_session / load_session / list     │
│  • drains prompt() SessionNotifications  │
│    → ChatEvent on the Channel            │
│  • permission_tx → ChatEvent::Permission │
└───────────┬─────────────────────────────┘
            │ ACP                                  app contributes:
            ▼                                      • spur-app.json mcp_server (wb_* tools)
   Agent subprocess (claude/codex)                 • skill/ context
     • calls app + foundation MCP tools  ──────────▶ notebook_push_source → panels cascade
     • streams AgentMessageChunk (answer)
```

The sidebar is **trusted React** (direct `invoke`/store access; no AFM iframe). The turn
engine reuses the `AcpAgentBackend` drain loop, but instead of returning a single
`String` it forwards each `SessionNotification` to the frontend `Channel` as a `ChatEvent`.

## 4. Components & file scope

**Frontend (jute-notebook/src):**
- `ui/notebook/sidebar/ChatPanel.tsx` — the panel (messages, streaming, inline permission,
  session picker, app-scope header).
- `ui/notebook/sidebar/panels.ts` — append one `SidebarPanel` entry `{ id: "agent",
  title: "AI Agent", icon: …, Component: ChatPanel }`.
- `stores/chat.ts` — chat store: per-app conversations, streaming buffer, pending
  permission, active session ref, app scope.
- subscribe to `notebook.store.viewState.path` (+ `viewMode`) to detect app switch.

**Backend (jute-notebook/src-tauri + spur-notebook):**
- `chat_turn` Tauri command (streaming, `Channel<ChatEvent>`).
- `chat_sessions_list` / `chat_switch_session` / `chat_new_session` / `chat_cancel`
  commands.
- a `SidebarChat` session manager (Rust) wrapping `AgentConnection`, reusing the
  `AcpAgentBackend` drain pattern, and the `permission_tx` route.
- an **app-context loader**: resolve the active app from the notebook path → read
  `spur-app.json` → produce `{ cwd, mcp_servers, skill }`; default "notebook app" when no
  manifest.

## 5. Session lifecycle (B + C-refresh, all native)

- **Per-app sessions (B):** sessions are scoped by `cwd` (the app dir / notebook dir).
  The picker lists them via the Orchestrator's `list_sessions_from_disk` +
  `list_sessions_from_rpc_with_cwd`, merged by `session_id` (the established pattern).
- **Switch back = resume:** `load_session(LoadSessionRequest)` returns a
  `Stream<SessionNotification>` that **replays history**; the manager **subscribes to the
  notification broadcast before** calling `load_session` (per the doc contract) so replay
  items are captured and rendered into the transcript.
- **Re-scope / first entry / tool change = C-refresh:** `new_session(cwd, mcp_servers)`
  with the app's tools. MCP servers bind at creation, so changing tools is a fresh
  `new_session`, never in-place mutation.
- **Resume capability gating:** if `SpurAgentCaps::supports_load_session` is false for the
  configured agent, the picker offers new sessions only (no resume) and says so.
- **One connection, many sessions:** sessions are `session_id`-namespaced on a single
  `AgentConnection`; persistence is native (agent-side + on disk). No custom session pool
  or N-subprocess fan-out.
- **UX:** mirror `SessionPickerView` — list, filter, switch with confirm
  (`ConfirmSwitchTarget::NewSession`), new.

## 6. App scope assembly

On notebook open and on `viewState.path` change, the app-context loader produces the
scope for the active app:

| Active surface | cwd | mcp_servers | skill / system context |
|---|---|---|---|
| Plain notebook ("notebook app", default) | notebook dir | foundation notebook tools (cells, datasources, `notebook_push_source`, `code_*`) | generic notebook-authoring guidance |
| A Spur App (`spur-app.json` present) | app dir | foundation tools **+** the app's `mcp_server` (e.g. workbench `wb_*`) | the app's `skill/SKILL.md` |

Switching apps triggers a **C-refresh** (or `load_session` if returning to an existing
app session). The chat header shows the current scope ("Notebook" vs the app name).

## 7. Streaming transport

`chat_turn(sessionRef, prompt, onEvent: Channel<ChatEvent>)` mirrors `run_cell`:

1. The manager ensures the app session (`load_session` resume or `new_session`), calls
   `prompt(PromptRequest)`, and drains the `SessionNotification` stream (reusing the
   `AcpAgentBackend` loop), forwarding each as a `ChatEvent`.
2. `ChatEvent` variants: `MessageChunk { text }`, `ToolCall { name, args_summary }`,
   `ToolResult { summary }`, `PermissionRequest { … }`, `Usage { input, output }`,
   `Done`, `Error { … }`.
3. The frontend `onEvent.onmessage` appends to the streaming buffer; tool calls render as
   chips; panels paint independently as the agent's tool calls hit `notebook_push_source`.
4. Cancellation: `chat_cancel` → `conn.cancel(session_id)`; already-streamed text and
   already-painted panels remain.

## 8. Permissions (identical to the TUI)

- **Interactive:** `permission_tx` (passed to `NativeAcpConnection::new`) delivers
  `PermissionRequest`; the manager forwards it as `ChatEvent::PermissionRequest`; the chat
  renders an inline approve/deny block (mirroring `App.pending_permission` +
  `SessionDetailView::push_permission`/`resolve_pending_permissions`); the decision
  resolves back through ACP.
- **Bypass:** honor the per-agent `AgentConfig.skip_permissions` /
  `skip_permissions_args` / `skip_permissions_session_mode` via `new_session_with_bypass`.
  If the user's agent is configured to skip, the sidebar skips identically.

## 9. App integration contract

An app integrates with the sidebar agent by providing two things (no agent code of its
own):

1. **MCP tools** — declared in `spur-app.json` `mcp_server` (the html_video / workbench
   plugin model). The sidebar agent receives them in `new_session(cwd, mcp_servers)`.
   Tools that paint the app call `notebook_push_source(port, ipc_bytes)` to cascade the
   app's panels (the agent-driven retrieval model from the workbench spec).
2. **Skill** — `skill/SKILL.md`, injected as the session's system/skill context so the
   agent knows the app's domain and tool repertoire.

The **Code Graph Workbench** is the exemplar: its `wb_*` evidence tools + skill make the
sidebar agent answer code questions and paint the graph/analyst/inspector panels. Its
former `wb-answer` AI node is removed in favor of the sidebar agent.

## 10. Error handling, lifecycle, teardown

- **Missing/!manifest:** falls back to the default notebook-app scope.
- **Agent spawn/prompt failure:** surfaced as `ChatEvent::Error` in the panel; the
  notebook and any already-painted app panels are unaffected.
- **No `supports_load_session`:** resume disabled; new sessions only.
- **Teardown:** `NativeAcpConnection` `Drop` + `.spur/pgids/` registry kill the agent
  subprocess on window close; broadcast capacity 4096 inherited (sizing invariant).
- **App switch mid-turn:** cancel the in-flight turn before re-scoping.

## 11. Testing

- **Session manager (Rust):** unit tests with the existing `MockConn`/`FakeConn`
  (`skip_perm_helper.rs`, `acp_backend.rs`) for `new_session` scope, `load_session` replay
  capture (subscribe-before-load), `list_sessions` cwd-merge, cancellation, and the
  permission route (interactive + bypass).
- **Streaming:** `chat_turn` forwards each `SessionNotification` kind to the correct
  `ChatEvent`; mirrors the `Channel` test pattern.
- **Frontend:** `ChatPanel` renders streaming, tool chips, inline permission grant/deny,
  and re-scopes when `viewState.path` changes (Vitest, like `NotebookSidebar.test.tsx`).
- **Integration:** opening the workbench app → asking a question → `wb_*` tool call →
  panels paint via `notebook_push_source` while the answer streams.

## 12. Plan preconditions & deferred

- **Task 0 — API-drift check:** re-verify `new_session`/`load_session`/`list_sessions`,
  `AcpAgentBackend`, `permission_tx`/`PermissionRequest`, `notebook_push_source`, and the
  sidebar registry against HEAD before building.
- **Net-new surface (the only real build):** `ChatPanel.tsx` + `stores/chat.ts`; the
  `chat_*` Tauri commands; the Rust `SidebarChat` session manager; the app-context loader.
- **Deferred:** Tier-2 attach to the orchestration brain session; multi-window session
  sharing; persisted transcript UI beyond what ACP `load_session` replay provides.
