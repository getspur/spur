# jute-notebook IPC unification audit

Date: 2026-05-28

Scope: read-only inventory of the current spur-notebook / jute-notebook IPC
surface. This document records current facts for a later synthesis pass. It does
not propose a refactor.

Graph-first note: the initial symbol anchors came from the SPUR code graph:

- `DaemonControlCommand`: `graph://symbol/99351023c20e0778`
- generated TS `DaemonControlCommand`: `graph://symbol/ff243563f80ca4a3`
- `daemon_control_request_from_legacy`: `graph://symbol/e07506935418d519`
- Unix `send_daemon_control` helper: `graph://symbol/43d371becea4899a`
- non-Unix `send_daemon_control` helper: `graph://symbol/c6688b4688c5b5c3`

The graph reported stale content for the command file, so all line citations
below were verified against the live worktree with `nl -ba`.

## 1. Legacy-string encoder call sites

Search terms used after graph lookup:

- `daemon_control_request_from_legacy`
- `send_daemon_control(`

The Unix helper is defined as:

- `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:538-547`
  - `send_daemon_control(command: &str, path: Option<&Path>, pinned: Option<bool>)`
  - derives the socket path from CLI args
  - calls `daemon_control_request_from_legacy(command, path, pinned)`
  - then calls `send_daemon_control_to(&socket_path, &request)`

The non-Unix helper is defined as:

- `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:549-558`
  - same signature
  - always returns `Error::NotebookDaemon`
  - does not call the legacy encoder

The legacy encoder itself is defined as:

- `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:587-623`
  - input: `command: &str`, `path: Option<&Path>`, `pinned: Option<bool>`
  - local `path_string()` converts `Option<&Path>` to display string or errors
  - `match command` constructs a typed `DaemonControlCommand`
  - wraps the typed command in `DaemonControlRequest::new(command)`

### Command: `list_recents`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1190-1214`
- Function:
  - `list_recent_notebooks`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"list_recents"`
- Arguments bundled into helper:
  - `path: None`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:606`
  - constructs `DaemonControlCommand::ListRecents {}`
- Result handling:
  - reads `response.entries.unwrap_or_default()`
  - adds Tauri-side `kernel_alive` and `is_current` metadata before returning
    `Vec<RecentNotebookEntry>`; see
    `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1195-1213`
- Direct typed-command observation:
  - This call has no payload fields. A direct
    `DaemonControlRequest::new(DaemonControlCommand::ListRecents {})` would
    carry the same information as the string helper path.

### Command: `remove_from_recents`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1216-1222`
- Function:
  - `remove_notebook_from_recents(path: String)`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"remove_from_recents"`
- Arguments bundled into helper:
  - `path: Some(Path::new(&path))`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:607-609`
  - constructs `DaemonControlCommand::RemoveFromRecents { path }`
- Result handling:
  - discards the daemon response body with `.map(|_| ())`
- Direct typed-command observation:
  - This call already has the path string from the Tauri command argument.
  - The legacy helper only converts `Path::new(&path)` back to a display string.

### Command: `set_pinned`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1224-1230`
- Function:
  - `set_notebook_pinned(path: String, pinned: bool)`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"set_pinned"`
- Arguments bundled into helper:
  - `path: Some(Path::new(&path))`
  - `pinned: Some(pinned)`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:610-614`
  - constructs `DaemonControlCommand::SetPinned { path, pinned }`
  - errors if `pinned` is missing
- Result handling:
  - discards the daemon response body with `.map(|_| ())`
- Direct typed-command observation:
  - This call already has both typed fields (`path: String`, `pinned: bool`).
  - The legacy helper adds only string dispatch and optionality checks.

### Command: `open`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1232-1239`
- Function:
  - `open_notebook_via_daemon(path: String)`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"open"`
- Arguments bundled into helper:
  - `path: Some(Path::new(&path))`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:597-599`
  - constructs `DaemonControlCommand::Open { path }`
- Result handling:
  - reads `response.path`
  - errors if the daemon response did not include `path`
- Direct typed-command observation:
  - This call already has the `path: String` needed by the enum variant.
  - The legacy helper only converts through `Path::new(&path).display()`.

### Command: `new`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1250-1257`
- Function:
  - `new_notebook_via_daemon()`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"new"`
- Arguments bundled into helper:
  - `path: None`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:600`
  - constructs `DaemonControlCommand::New {}`
- Result handling:
  - reads `response.path`
  - errors if the daemon response did not include `path`
- Direct typed-command observation:
  - This call has no payload fields. A direct
    `DaemonControlRequest::new(DaemonControlCommand::New {})` would carry the
    same information.

### Command: `new_at`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1259-1266`
- Function:
  - `new_notebook_at_via_daemon(path: String)`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"new_at"`
- Arguments bundled into helper:
  - `path: Some(Path::new(&path))`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:601-603`
  - constructs `DaemonControlCommand::NewAt { path }`
- Result handling:
  - reads `response.path`
  - errors if the daemon response did not include `path`
- Direct typed-command observation:
  - This call already has the `path: String` needed by the enum variant.

### Command: `reopen`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1268-1275`
- Function:
  - `reopen_notebook_via_daemon()`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"reopen"`
- Arguments bundled into helper:
  - `path: None`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:604`
  - constructs `DaemonControlCommand::Reopen {}`
- Result handling:
  - reads `response.path`
  - errors if the daemon response did not include `path`
- Direct typed-command observation:
  - This call has no payload fields. A direct typed request would carry the
    same information.

### Command: `close`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1277-1281`
- Function:
  - `close_notebook_via_daemon()`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- String command passed:
  - `"close"`
- Arguments bundled into helper:
  - `path: None`
  - `pinned: None`
- Legacy encoder arm:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:605`
  - constructs `DaemonControlCommand::Close {}`
- Result handling:
  - discards the daemon response body with `.map(|_| ())`
- Direct typed-command observation:
  - This call has no payload fields. A direct typed request would carry the
    same information.

### Related typed path: `rename`

- Call site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1241-1248`
- Function:
  - `rename_notebook(from: String, to: String)`
- Tauri registration:
  - `crates/spur-notebook/src/main.rs:277-309`
- It does not call `send_daemon_control(`.
- It calls `send_daemon_control_rename(Path::new(&from), Path::new(&to))`.
- Typed request constructor:
  - `daemon_control_rename_request(from, to)` constructs
    `DaemonControlCommand::Rename { from, to }`;
    `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:560-565`
- Socket send:
  - `send_daemon_control_rename` calls `send_daemon_control_to` directly with
    the typed request;
    `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:567-575`
- Direct typed-command observation:
  - `rename` is already outside the string trampoline path.

### Encoder arms with no current helper call site

- `shutdown`
  - Legacy encoder arm:
    `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:615`
  - No `send_daemon_control("shutdown", ...)` call was found under
    `crates/spur-notebook/`.
- Open question:
  - Is `shutdown` exercised by a different client that writes the legacy daemon
    command envelope directly, or is it currently unused by this app path?

## 2. DaemonControlCommand variant inventory

Enum declaration:

- `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:118-179`
- Attributes:
  - `#[derive(Debug, Clone, Serialize, Deserialize, TS)]`
  - `#[serde(tag = "command", rename_all = "snake_case")]`
  - `#[ts(rename_all = "snake_case")]`
- Generated TS binding:
  - `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:7-37`

### Variant table

| Variant | Rust fields | Wire command / serde rename | TS binding line | Legacy encoder arm | Current reachability notes |
|---|---|---|---|---|---|
| `Open` | `path: String` | default snake-case: `"open"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:8` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:597-599` | Reached by `open_notebook_via_daemon`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1232-1239`. |
| `Rename` | `from: String`, `to: String` | default snake-case: `"rename"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:9` | none in `daemon_control_request_from_legacy`; typed helper exists | Reached by `rename_notebook` through `daemon_control_rename_request`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1241-1248` and `:560-575`. |
| `New` | none | default snake-case: `"new"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:10` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:600` | Reached by `new_notebook_via_daemon`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1250-1257`. |
| `NewAt` | `path: String` | explicit `#[serde(rename = "new_at")]` and `#[ts(rename = "new_at")]` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:11` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:601-603` | Reached by `new_notebook_at_via_daemon`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1259-1266`. |
| `Reopen` | none | default snake-case: `"reopen"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:12` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:604` | Reached by `reopen_notebook_via_daemon`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1268-1275`. |
| `Close` | none | default snake-case: `"close"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:13` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:605` | Reached by `close_notebook_via_daemon`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1277-1281`. |
| `ListRecents` | none | default snake-case: `"list_recents"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:14` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:606` | Reached by `list_recent_notebooks`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1190-1214`. |
| `RemoveFromRecents` | `path: String` | default snake-case: `"remove_from_recents"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:15` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:607-609` | Reached by `remove_notebook_from_recents`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1216-1222`. |
| `SetPinned` | `path: String`, `pinned: bool` | default snake-case: `"set_pinned"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:16` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:610-614` | Reached by `set_notebook_pinned`; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1224-1230`. |
| `Shutdown` | none | default snake-case: `"shutdown"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:17` | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:615` | No `send_daemon_control("shutdown", ...)` call found under `crates/spur-notebook/`. |
| `WriteCell` | `id: String`, `source: String`, `expected_version: Option<u64>`, `last_edited_by: Option<String>` | default snake-case: `"write_cell"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:18-24` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon` for daemon control requests; `crates/spur-notebook/src/mcp/mod.rs:1090-1099`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:670-692`. |
| `ReadCell` | `id: String` | default snake-case: `"read_cell"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:25` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1090-1102`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:693-697`. |
| `InsertCell` | `kind: CellKind`, `after_id: Option<String>`, `source: String`, `last_edited_by: Option<String>` | default snake-case: `"insert_cell"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:26-32` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1090-1110`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:698-719`. |
| `LoadNotebook` | `path: String` | explicit `#[serde(rename = "load")]` and `#[ts(rename = "load")]` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:33` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1111-1118`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:720-729`. |
| `DeleteCell` | `id: String`, `expected_version: u64` | default snake-case: `"delete_cell"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:34` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1119-1124`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:730-742`. |
| `Snapshot` | none | default snake-case: `"snapshot"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:35` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1125`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:743-749`. |
| `ApplyEdit` | `id: String`, `source: String` | default snake-case: `"apply_edit"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:36` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1126-1129`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:750-756`. |
| `FlushNotebook` | none | default snake-case: `"flush_notebook"` | `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts:37` | none in `daemon_control_request_from_legacy` | Reached through `notebook_store_request_from_daemon`; `crates/spur-notebook/src/mcp/mod.rs:1130`. Handled by notebook store; `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:757-761`. |

### Variant reachability flags

- In enum but no corresponding `daemon_control_request_from_legacy` arm:
  - `Rename`
  - `WriteCell`
  - `ReadCell`
  - `InsertCell`
  - `LoadNotebook`
  - `DeleteCell`
  - `Snapshot`
  - `ApplyEdit`
  - `FlushNotebook`
- In enum and in `daemon_control_request_from_legacy`, but no current
  `send_daemon_control` call found:
  - `Shutdown`
- Otherwise reachable from the legacy helper path:
  - `Open`
  - `New`
  - `NewAt`
  - `Reopen`
  - `Close`
  - `ListRecents`
  - `RemoveFromRecents`
  - `SetPinned`
- Otherwise reachable from a typed request path:
  - `Rename`
- Otherwise reachable through the notebook-store daemon bridge:
  - `WriteCell`
  - `ReadCell`
  - `InsertCell`
  - `LoadNotebook`
  - `DeleteCell`
  - `Snapshot`
  - `ApplyEdit`
  - `FlushNotebook`

## 3. Tauri events currently emitted

Search terms:

- `.emit(`
- `app.emit(`
- `app_handle.emit(`
- `Emitter::emit`

### Event: `notebook://changed`

- Emit site:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs:22-58`
- Event name:
  - constant `NOTEBOOK_CHANGED_EVENT = "notebook://changed"`
  - `crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs:22`
- Payload type:
  - `NotebookDelta`
  - generated TS binding says it has `version: number` and `kind: DeltaKind`;
    `crates/spur-notebook/jute-notebook/src/bindings/NotebookDelta.ts:4-16`
- Producer:
  - `spawn_notebook_delta_forwarder` subscribes to `state.get_notebook().subscribe()`
  - on each broadcast `Ok(delta)`, emits the delta through Tauri
  - `crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs:26-58`
- Consumer:
  - `listenForNotebookEvents` listens as `listen<NotebookDelta>("notebook://changed", ...)`
  - it calls `reconcileNotebookDelta(notebook, event.payload)`
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:84-93`
- Pipeline classification:
  - This is the delta-forwarding pipeline.
  - It bridges tokio broadcast to Tauri events.

### Event: `notebook://run_cell_event`

- Emit site:
  - `crates/spur-notebook/src/mcp/tools/run_cell.rs:105-127`
- Event name:
  - constant `RUN_CELL_EVENT_NAME = "notebook://run_cell_event"`
  - `crates/spur-notebook/src/mcp/tools/run_cell.rs:14`
- Payload shape:
  - JSON object with:
    - `cell_id`
    - `kernel_id`
    - `event`
  - emitted at `crates/spur-notebook/src/mcp/tools/run_cell.rs:119-126`
- Payload nested type:
  - `event` is serialized from `RunCellEvent`
  - generated binding enumerates `started`, `stdout`, `stderr`,
    `execute_result`, `display_data`, `update_display_data`, `clear_output`,
    `error`, `disconnect`, and `finished`;
    `crates/spur-notebook/jute-notebook/src/bindings/RunCellEvent.ts:7-32`
- Producer:
  - MCP `notebook.run_cell` drains kernel events and emits each event when an
    app handle is available.
  - The same loop also applies the event to the notebook store:
    `crates/spur-notebook/src/mcp/tools/run_cell.rs:116-134`
- Consumer:
  - `listenForNotebookEvents` listens as
    `listen<RunCellEventPayload>("notebook://run_cell_event", ...)`
  - when the in-process store is enabled, the consumer returns early
  - when the in-process store is disabled, it calls
    `notebook.handleRunCellEvent(...)`
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:63-72`
- Pipeline classification:
  - Ad-hoc UI event for kernel run output in the non-in-proc-store path.
  - Related to kernel output, but not the `NotebookDelta` broadcast-forwarding
    path.

### Event: `agent://request`

- Emit site:
  - `crates/spur-notebook/src/mcp/bridge.rs:252-270`
- Event name:
  - literal `"agent://request"`
  - `crates/spur-notebook/src/mcp/bridge.rs:262`
- Payload type:
  - Rust `BridgeRequest` at the emit site
  - TS `AgentBridgeRequest` is a discriminated union by `method`;
    `crates/spur-notebook/jute-notebook/src/agent/types.ts:4-40`
- Producer:
  - Rust `AgentBridge` inserts a pending oneshot by request id, emits the
    request to the frontend, and waits for `agent_response`.
  - `crates/spur-notebook/src/mcp/bridge.rs:252-270`
- Consumer:
  - `registerAgentBridge` listens as
    `listen<AgentBridgeRequest>("agent://request", async (event) => ...)`
  - it dispatches to frontend notebook handlers and replies via
    `invoke("agent_response", { payload: response })`
  - `crates/spur-notebook/jute-notebook/src/agent/bridge.ts:32-38`
- Pipeline classification:
  - Ad-hoc agent bridge request/response event.
  - It is not part of the notebook delta-forwarding pipeline.

### Event: `notebook://saved`

- Emit site:
  - `crates/spur-notebook/src/mcp/tools/save.rs:78-98`
- Event name:
  - literal `"notebook://saved"`
  - `crates/spur-notebook/src/mcp/tools/save.rs:90`
- Payload shape:
  - JSON object `{ "path": saved_path }`
  - `crates/spur-notebook/src/mcp/tools/save.rs:89-90`
- Producer:
  - MCP `notebook.save` saves notebook contents, then emits the saved path when
    an app handle is available.
  - `crates/spur-notebook/src/mcp/tools/save.rs:78-98`
- Consumer:
  - `listenForNotebookEvents` listens as
    `listen<SavedPayload>("notebook://saved", ...)`
  - current handler only references the payload and has a TODO about dirty state
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:78-81`
- Pipeline classification:
  - Ad-hoc UI event for save notification state.

### Event: `notebook://recents_changed`

- Event name:
  - constant `RECENTS_CHANGED_EVENT = "notebook://recents_changed"`
  - `crates/spur-notebook/src/mcp/tools/mod.rs:33-35`
- Emit site A:
  - `crates/spur-notebook/src/mcp/tools/mod.rs:170-174`
- Payload shape A:
  - JSON object `{}`
  - `crates/spur-notebook/src/mcp/tools/mod.rs:170-173`
- Producer A:
  - helper `emit_recents_changed(deps)`
- Emit site B:
  - `crates/spur-notebook/src/mcp/mod.rs:454-458`
- Payload shape B:
  - JSON object `{}`
  - `crates/spur-notebook/src/mcp/mod.rs:454-458`
- Producer B:
  - app-backed daemon implementation method `emit_recents_changed`
- Consumer:
  - `listenForRecentNotebookChanges` listens as
    `listen("notebook://recents_changed", ...)`
  - handler calls `refreshRecents`
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:98-108`
  - Home page registers that listener and also refreshes on window focus;
    `crates/spur-notebook/jute-notebook/src/pages/HomePage.tsx:277-304`
- Pipeline classification:
  - Ad-hoc UI event for recents refresh.

### Potential orphan emit/listen observations

- No Rust emit site for `notebook://kernel_changed` was found under
  `crates/spur-notebook/`.
- A TS listener exists for `notebook://kernel_changed`;
  `crates/spur-notebook/jute-notebook/src/agent/events.ts:73-77`
- Open question:
  - Is `notebook://kernel_changed` emitted by code outside
    `crates/spur-notebook/`, by a plugin, or is it currently an orphan listener?

## 4. Tauri events currently listened-to (TS side)

Search terms:

- `listen<`
- `listen(`
- `getCurrentWindow().listen`

No `getCurrentWindow().listen` call was found in the searched frontend paths.

### Listener: `notebook://run_cell_event`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:63-72`
- Event name:
  - `"notebook://run_cell_event"`
- Handler signature:
  - `listen<RunCellEventPayload>(..., (event) => { ... })`
- Payload type:
  - `RunCellEventPayload`
  - local type with `cell_id: string`, `kernel_id: string`, `event: RunCellEvent`
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:10-14`
- Paired emit:
  - `crates/spur-notebook/src/mcp/tools/run_cell.rs:119-126`
- Handler behavior:
  - loads runtime config
  - ignores event if `inProcStore` is true
  - ignores event for another kernel id
  - applies event to the notebook UI state
- Orphan status:
  - paired.

### Listener: `notebook://kernel_changed`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:73-77`
- Event name:
  - `"notebook://kernel_changed"`
- Handler signature:
  - `listen("notebook://kernel_changed", () => { ... })`
- Payload type:
  - none used
- Paired emit:
  - no `.emit("notebook://kernel_changed", ...)` or equivalent constant use was
    found under `crates/spur-notebook/`
- Handler behavior:
  - calls `notebook.refreshKernelSlotInfo()`
- Orphan status:
  - orphan listener in the searched scope.
- Open question:
  - Is this event emitted outside the searched crates or planned for a future
    kernel lifecycle emitter?

### Listener: `notebook://saved`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:78-81`
- Event name:
  - `"notebook://saved"`
- Handler signature:
  - `listen<SavedPayload>(..., (event) => { ... })`
- Payload type:
  - local `SavedPayload` with optional `path?: string`
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:16-18`
- Paired emit:
  - `crates/spur-notebook/src/mcp/tools/save.rs:89-96`
- Handler behavior:
  - currently discards payload
  - TODO says dirty-state clearing is pending
- Orphan status:
  - paired, but handler is currently observational/no-op.

### Listener: `notebook://changed`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:84-93`
- Event name:
  - `"notebook://changed"`
- Handler signature:
  - `listen<NotebookDelta>(..., (event) => { ... })`
- Payload type:
  - `NotebookDelta`
  - `crates/spur-notebook/jute-notebook/src/bindings/NotebookDelta.ts:4-16`
- Paired emit:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs:42-49`
- Handler behavior:
  - calls `reconcileNotebookDelta(notebook, event.payload)`
  - reconcile returns early if in-proc store is disabled;
    `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:662-668`
  - for loaded deltas, it fetches `notebook_store_snapshot`;
    `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:849-856`
  - for written/inserted cells, it fetches individual cells with
    `read_notebook_store_cell`;
    `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:856-859` and
    `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:1032-1035`
- Orphan status:
  - paired.

### Listener: `notebook://recents_changed`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/events.ts:98-108`
- Event name:
  - `"notebook://recents_changed"`
- Handler signature:
  - `listen("notebook://recents_changed", () => { ... })`
- Payload type:
  - none used
- Paired emits:
  - helper emit:
    `crates/spur-notebook/src/mcp/tools/mod.rs:170-173`
  - app-backed daemon emit:
    `crates/spur-notebook/src/mcp/mod.rs:454-458`
- Handler behavior:
  - calls the supplied `refreshRecents` callback
  - Home page callback invokes `list_recent_notebooks`;
    `crates/spur-notebook/jute-notebook/src/pages/HomePage.tsx:277-289`
- Orphan status:
  - paired.

### Listener: `agent://request`

- Listen site:
  - `crates/spur-notebook/jute-notebook/src/agent/bridge.ts:32-38`
- Event name:
  - `"agent://request"`
- Handler signature:
  - `listen<AgentBridgeRequest>(..., async (event) => { ... })`
- Payload type:
  - `AgentBridgeRequest`
  - TS union by `method`;
    `crates/spur-notebook/jute-notebook/src/agent/types.ts:4-40`
- Paired emit:
  - `crates/spur-notebook/src/mcp/bridge.rs:252-270`
- Handler behavior:
  - `handleAgentRequest(event.payload)`
  - sends reply through Tauri command
    `invoke("agent_response", { payload: response })`
  - signals readiness through `invoke("bridge_ready")`
- Orphan status:
  - paired.

### Orphan summary

| Event | Emit found? | Listen found? | Status |
|---|---:|---:|---|
| `notebook://changed` | yes | yes | paired |
| `notebook://run_cell_event` | yes | yes | paired |
| `agent://request` | yes | yes | paired |
| `notebook://saved` | yes | yes | paired |
| `notebook://recents_changed` | yes | yes | paired |
| `notebook://kernel_changed` | no | yes | orphan listener in searched scope |

## 5. Tauri 2 IPC capabilities

Docs fetched:

- `https://v2.tauri.app/develop/calling-rust/`
- `https://v2.tauri.app/develop/calling-frontend/`
- `https://v2.tauri.app/reference/javascript/api/namespaceevent/`
- `https://v2.tauri.app/reference/javascript/api/namespacecore/`
- `https://docs.rs/tauri/latest/tauri/ipc/struct.Channel.html`
- `https://docs.rs/tauri/latest/tauri/ipc/struct.Response.html`
- `https://docs.rs/tauri/latest/tauri/ipc/struct.Request.html`
- `https://docs.rs/tauri/latest/src/tauri/ipc/channel.rs.html`

Current dependency versions in this workspace:

- Rust crates request `tauri = { version = "2.0.4", ... }`;
  `crates/spur-notebook/Cargo.toml:23` and
  `crates/spur-notebook/jute-notebook/src-tauri/Cargo.toml:34`
- `Cargo.lock` currently resolves Rust `tauri` to `2.11.2`;
  `Cargo.lock:12080-12084`
- frontend package requests `@tauri-apps/api` `^2.11.0`;
  `crates/spur-notebook/jute-notebook/package.json:28`

### Commands / `invoke`

- Capability:
  - JS calls Rust commands by `invoke(cmd, args, options?)`.
  - Command handlers can accept arguments and return values.
  - Arguments deserialize via Serde; returned data serializes via Serde.
- Docs:
  - `https://v2.tauri.app/develop/calling-rust/`
  - `https://v2.tauri.app/reference/javascript/api/namespacecore/`
- Since:
  - JS `invoke` is marked `Since 1.0.0` in the core API reference.
- Prerequisites:
  - Rust command annotated with `#[tauri::command]`
  - command included in one `tauri::generate_handler![...]`
  - frontend imports `invoke` from `@tauri-apps/api/core` or uses
    `window.__TAURI__.core.invoke` when `app.withGlobalTauri` is enabled
- Transport:
  - The public docs describe this as IPC from frontend to backend.
  - The docs page does not give one stable transport string for every platform.
  - The raw request example uses `__TAURI__.core.invoke` for the global API.
- Known limits:
  - Normal command return values are serialized to JSON.
  - Docs say JSON serialization can slow large values and point to
    `tauri::ipc::Response` for array buffers.
  - Async command borrowed arguments have limitations documented on the calling
    Rust page.

### Typed event API (`emit`, `listen`, `emitTo`, `once`)

- Capability:
  - JS API exposes generic TypeScript signatures such as
    `emit<T>(event, payload?)` and `listen<T>(event, handler, options?)`.
  - `Event<T>` exposes `payload: T`.
- Docs:
  - `https://v2.tauri.app/reference/javascript/api/namespaceevent/`
  - `https://v2.tauri.app/develop/calling-frontend/`
- Since:
  - `emit` is marked `Since 1.0.0`.
  - `listen` is marked `Since 1.0.0`.
  - `emitTo` is marked `Since 2.0.0`.
- Prerequisites:
  - frontend imports from `@tauri-apps/api/event` or uses
    `window.__TAURI__.event` when `app.withGlobalTauri` is enabled.
  - Rust side uses event-system traits implemented by `AppHandle` and
    `WebviewWindow`.
- Transport:
  - Tauri docs say the event system directly evaluates JavaScript under the
    hood.
- Known limits:
  - Tauri docs say events are not designed for low latency or high throughput.
  - Tauri docs say event payloads have no strong type support and are always
    JSON strings.
  - Tauri docs say event payloads are not suitable for bigger messages.
  - Event names must include only alphanumeric characters, `-`, `/`, `:`, and
    `_`.

### `Channel<T>` streams

- Capability:
  - JS can create `new Channel<T>()`, set `onmessage`, and pass the channel as a
    command argument.
  - Rust command can receive `tauri::ipc::Channel<T>` and call `.send(data)`.
  - Docs describe channels as the recommended mechanism for streaming data and
    say they are designed to be fast and ordered.
- Docs:
  - `https://v2.tauri.app/develop/calling-rust/`
  - `https://v2.tauri.app/develop/calling-frontend/`
  - `https://v2.tauri.app/reference/javascript/api/namespacecore/`
  - `https://docs.rs/tauri/latest/tauri/ipc/struct.Channel.html`
- Since:
  - The JS `Channel<T>` class appears in the Tauri 2 core API reference.
  - The reference page does not show a `Since` marker directly on the class in
    the fetched content; unverified.
  - Tauri v2 beta announcement described a new channel API, but that blog was
    not used as a primary fact source for this audit.
- Prerequisites:
  - frontend imports `Channel` from `@tauri-apps/api/core`
  - Rust command argument type includes `tauri::ipc::Channel<T>`
  - `T` sent by Rust must implement Tauri `IpcResponse`
  - for typed Rust payloads, payload type normally implements `Serialize`
- Transport:
  - Tauri guide says channels are used internally for streamed HTTP responses,
    child process output, and WebSocket messages.
  - Tauri source shows small JSON channel payloads are delivered via
    `webview.eval`.
  - Tauri source shows larger payloads are stored in `ChannelDataIpcQueue` and
    fetched through `window.__TAURI_INTERNALS__.invoke(...)`.
- Known limits:
  - Tauri source defines direct-send thresholds:
    - JSON direct execute threshold: 8192 bytes
    - raw direct execute threshold: 1024 bytes
  - Larger payloads use the fetch path through an internal command.
  - Public docs do not state an application-facing max payload size in the
    fetched pages; unverified.

### `tauri::ipc::Response`

- Capability:
  - Rust command can return `tauri::ipc::Response` to send an IPC response body,
    including array-buffer-style raw data.
- Docs:
  - `https://v2.tauri.app/develop/calling-rust/`
  - `https://docs.rs/tauri/latest/tauri/ipc/struct.Response.html`
- Since:
  - The fetched docs.rs page for `Response` does not show a `Since` marker;
    unverified.
- Prerequisites:
  - command returns `Response`
  - body can be converted into `InvokeResponseBody`
- Transport:
  - command response over Tauri IPC.
- Known limits:
  - The guide positions `Response` as an optimized path for large data compared
    with JSON serialization.
  - Exact max response size was not found in the fetched docs; unverified.

### `tauri::ipc::Request` and `InvokeBody`

- Capability:
  - Rust command can accept `tauri::ipc::Request`.
  - Request exposes raw body and headers.
  - Guide example matches `tauri::ipc::InvokeBody::Raw(upload_data)`.
- Docs:
  - `https://v2.tauri.app/develop/calling-rust/`
  - `https://docs.rs/tauri/latest/tauri/ipc/struct.Request.html`
- Since:
  - `InvokeArgs` in JS core API is marked `Since 1.0.0`.
  - `InvokeOptions` with headers is marked `Since 2.0.0`.
  - Rust `Request` docs do not show a `Since` marker in fetched content;
    unverified.
- Prerequisites:
  - frontend can call `invoke` with `ArrayBuffer` or `Uint8Array` payload and
    headers in the third argument.
  - Rust command receives `tauri::ipc::Request`.
- Transport:
  - command request over Tauri IPC.
- Known limits:
  - Request docs say raw bytes are supported on all platforms except Android.
  - Public docs do not state a max raw request size in fetched pages; unverified.

### `js-sys` mention

- The fetched Tauri 2 pages above did not surface a `js-sys` IPC primitive by
  name.
- Open question:
  - Which exact `js-sys` capability was intended for this audit item:
    webview-side `ArrayBuffer`/`Uint8Array`, wasm bindings, or a Rust-side
    Tauri implementation detail?
- Status:
  - unverified from fetched Tauri docs.

## 6. Mapping table

| Current channel | Carries | Framing today | Possible Tauri-2 primitive | Constraints / blockers |
|---|---|---|---|---|
| Tauri `invoke` from Home page to Rust command, then Unix socket daemon control | Open existing notebook from a recent entry or file picker | TS calls `invoke<string>("open_notebook_via_daemon", { path })`; Rust call wraps `"open"` plus `Some(Path::new(&path))`; legacy encoder creates `DaemonControlCommand::Open { path }`; Unix socket sends length-prefixed JSON request; response includes `path`. Citations: `crates/spur-notebook/jute-notebook/src/pages/HomePage.tsx:329-346`, `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1232-1239`, `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:597-599`, `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:452-514`. | Tauri command result; command could return typed `Result<String, Error>` as it does now. Tauri docs also provide `ipc::Response` for raw/binary responses, but open notebook currently returns a string. | Socket still crosses process boundary to daemon. Open question: keep daemon process boundary vs express this flow as a single Tauri command path when the daemon is in the same Tauri process. |
| MCP server / agent bridge event plus Tauri `invoke` reply | `write_cell` mutation requested by notebook MCP tool | MCP tool `notebook.write_cell` builds JSON params and calls `bridge.request("notebook.write_cell", ...)`; Rust emits `agent://request`; TS listens and dispatches to frontend handler; TS replies via `invoke("agent_response", { payload })`. Citations: `crates/spur-notebook/src/mcp/tools/write_cell.rs:43-84`, `crates/spur-notebook/src/mcp/bridge.rs:252-270`, `crates/spur-notebook/jute-notebook/src/agent/bridge.ts:32-38`, `crates/spur-notebook/src/main.rs:307-309`. | Tauri command with `Channel<T>` for streaming progress is available, but this flow is currently request/reply. Typed event payloads are available on the TS side but Tauri docs say events have no strong type support at runtime. | This flow depends on active frontend notebook state. Open question: frontend-owned mutation state vs in-process store mutation path. |
| Tauri command result stream using `Channel<RunCellEvent>` | Direct UI kernel output delta while user runs a cell | TS creates `new Channel<RunCellEvent>()`, assigns `onmessage`, then passes `onEvent` into `invoke("run_cell", ...)`; Rust command sends messages on that channel. Citations: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:939-955`, `crates/spur-notebook/src/main.rs:292-296`, Tauri channel docs at `https://v2.tauri.app/develop/calling-rust/` and `https://v2.tauri.app/develop/calling-frontend/`. | Already uses Tauri 2 `Channel<T>` for command result streaming. | This is direct UI run-cell output, separate from MCP `notebook://run_cell_event` and notebook-store `NotebookDelta::runCellEvent` propagation. Open question: direct channel vs store delta vs event fallback for every run-cell producer. |
| Tauri event | Recents change notification | Rust emits `notebook://recents_changed` with `{}` from two sites; TS listens and refreshes recents by invoking `list_recent_notebooks`, which itself calls socket command `"list_recents"`. Citations: `crates/spur-notebook/src/mcp/tools/mod.rs:33-35`, `crates/spur-notebook/src/mcp/tools/mod.rs:170-173`, `crates/spur-notebook/src/mcp/mod.rs:454-458`, `crates/spur-notebook/jute-notebook/src/agent/events.ts:98-108`, `crates/spur-notebook/jute-notebook/src/pages/HomePage.tsx:277-304`, `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1190-1214`. | Tauri event remains available; a Tauri command returning recents is already present; a channel could stream recents changes if ordered high-throughput updates mattered. | Current payload is empty, so listener must refetch. Open question: empty invalidation event vs typed payload containing changed recents. |
| tokio broadcast to Tauri event | Notebook delta broadcast after in-process store mutation | Rust `spawn_notebook_delta_forwarder` subscribes to notebook broadcast receiver and emits `notebook://changed` for each `NotebookDelta`; TS listens and reconciles by fetching snapshots or cells as needed. Citations: `crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs:26-58`, `crates/spur-notebook/jute-notebook/src/agent/events.ts:84-93`, `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:662-668`, `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:849-866`, `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:1032-1035`. | Tauri docs identify `Channel<T>` as fast and ordered for streaming backend-to-frontend data; Tauri events are available but documented as not designed for high throughput. | Current producer is a process-wide broadcast receiver, not a command invocation with a caller-supplied channel. Open question: long-lived subscription command/channel vs global event forwarder. |

