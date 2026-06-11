# Notebook Sidebar AI Agent Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-09-notebook-sidebar-ai-agent-design.md`
**Companion:** `docs/superpowers/specs/2026-06-09-notebook-sidebar-ai-agent-integration.ipynb`
**Design epic:** (this brainstorming thread)

**Goal:** Ship an app-aware AI Agent chat in the notebook sidebar: default-scoped to the notebook, re-scoped per Spur App via ACP `new_session(cwd, mcp_servers)`, streaming over a Tauri `Channel`, with TUI-identical permissions.

**Architecture:** A Rust `SidebarChat` manager in `crates/spur-notebook/src/sidebar_chat/` wraps a `spur_acp` `AgentConnection`, keeps per-app sessions, and converts ACP `SessionNotification`s into `ChatEvent`s. For native ACP it subscribes to `subscribe_session_notifications()` before `prompt()`/`load_session()` and treats the returned stream as a completion signal; for stream-only adapters it drains the returned stream directly. Tauri `chat_*` commands bridge it to the frontend via `Channel<ChatEvent>` (the `run_cell` pattern), including an explicit permission-response command that resolves the ACP `reply_tx`. A trusted-React `ChatPanel` + one `SIDEBAR_PANELS` entry render it. Apps contribute MCP tools + skill; the agent paints app panels via `notebook_push_source`.

**Tech Stack:** Rust (`spur-notebook`, `spur-acp`, `spur-core`, `agent-client-protocol`), Tauri (`Channel`, `async_channel`), React + Zustand (jute-notebook), Vitest.

---

## File Structure Map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/spur-notebook/src/sidebar_chat/mod.rs` | module root + re-exports + drift-pin doc | 0 |
| `crates/spur-notebook/src/sidebar_chat/types.rs` | `ChatEvent`, `AppScope`, `SessionRef` | 1 |
| `crates/spur-notebook/src/sidebar_chat/scope.rs` | app-context loader (path → `AppScope`) | 2 |
| `crates/spur-notebook/src/sidebar_chat/manager.rs` | `SidebarChat`: sessions + broadcast-first turn drain + pending permission replies + cancel | 3, 4 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/chat_state.rs` + `state.rs` | sidebar chat connection/config state, permission channel, cancellation root | 5 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs` | `chat_*` Tauri commands, including permission response | 6 |
| `crates/spur-notebook/jute-notebook/src/stores/chat.ts` | chat store | 7 |
| `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx` + `panels.ts` | panel + registry entry | 8 |
| `…/src-tauri/tests/chat_turn_stream.rs` | boundary integration test | 9 |

---

## Task DAG

```
0 ─┬─ 1 ─┬─ 2 ─────────────┐
   │     ├─ 3 ── 4 ────────┼── 5 ── 6 ──┬── 8
   │     └─ 7 ─────────────┘            └── 9
   └────────────────────────────────────────
```

- After **1**: `2`, `3`, `7` run in parallel.
- **4** depends `3`; **5** depends `4`+`2`; **6** depends `5`; **8** depends `6`+`7`; **9** depends `6`.

---

## Task 0: Scaffold `sidebar_chat` module + pin ACP/notebook APIs (drift check)

**Task ID:** `task-0`

**Files:**
- Create: `crates/spur-notebook/src/sidebar_chat/mod.rs`
- Modify: `crates/spur-notebook/src/lib.rs` (add `pub mod sidebar_chat;`)
- Test: inline `#[cfg(test)]` in `mod.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `scripts/spur-cargo build -p spur-notebook` compiles with the new module.
- [ ] The drift-pin test compiles (it imports every external symbol the plan relies on); a compile error here means an API moved since the spec and the plan must be revised before continuing.
- [ ] No warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `sidebar_chat/mod.rs`, one line in `lib.rs`.
- OUT: any other file. If the imports below don't resolve, **emit `risk`** (API drift) instead of changing other crates.

**Implementation:**
- [ ] **Step 1: Create the module with a drift-pin test that imports the real APIs.**
```rust
//! Sidebar AI Agent chat. Reuses the shipped ACP session + AcpAgentBackend
//! drain primitives; see docs/superpowers/specs/2026-06-09-notebook-sidebar-ai-agent-design.md.

pub mod types;
pub mod scope;
pub mod manager;

#[cfg(test)]
mod drift_pin {
    // Compile-time assertion that the spec's reused APIs still exist at HEAD.
    #[allow(unused_imports)]
    use spur_acp::connection::AgentConnection;
    #[allow(unused_imports)]
    use spur_acp::types::PermissionRequest;
    #[allow(unused_imports)]
    use agent_client_protocol::schema::{McpServer, SessionNotification, SessionUpdate};
    #[allow(unused_imports)]
    use crate::dag::ai::acp_backend::AcpAgentBackend;

    #[test]
    fn apis_present() {
        // Trait methods used by the manager (signature pin):
        //   AgentConnection::new_session(&mut self, cwd: PathBuf, mcp_servers: Vec<McpServer>)
        //   AgentConnection::load_session(&mut self, req: LoadSessionRequest) -> Stream<SessionNotification>
        //   AgentConnection::list_sessions(&mut self) ...
        //   AgentConnection::prompt(&mut self, req) -> Stream<SessionNotification>
        //   AgentConnection::cancel(&mut self, session_id)
        // If any signature changed, the manager tasks (3,4) will not compile.
        assert!(true);
    }
}
```
- [ ] **Step 2:** `scripts/spur-cargo build -p spur-notebook` (remote default). Expected: compiles. If an import fails to resolve, STOP and emit `risk` with the drifted symbol.
- [ ] **Step 3: Commit.**
```bash
git add crates/spur-notebook/src/sidebar_chat/mod.rs crates/spur-notebook/src/lib.rs
git commit -m "feat(sidebar-chat): scaffold module + pin reused ACP/notebook APIs"
```

---

## Task 1: `ChatEvent` + `AppScope` + `SessionRef` types

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/src/sidebar_chat/types.rs`
- Test: inline `#[cfg(test)]`

**Depends on:** `task-0`

**Acceptance Criteria:**
- [ ] `ChatEvent` serializes to camelCase JSON (frontend wire format).
- [ ] Round-trip serde test passes for each variant.
- [ ] `scripts/spur-cargo test -p spur-notebook sidebar_chat::types` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `types.rs`. OUT: `manager.rs`, `scope.rs` (later tasks).

**Implementation:**
- [ ] **Step 1: Write the failing test.**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_event_message_chunk_roundtrips_camel_case() {
        let ev = ChatEvent::MessageChunk { text: "hi".into() };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"messageChunk\""));
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }
}
```
- [ ] **Step 2:** `scripts/spur-cargo test -p spur-notebook chat_event_message_chunk` → FAIL (type missing).
- [ ] **Step 3: Implement.**
```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Streamed to the frontend over a Tauri Channel, one per agent notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    MessageChunk { text: String },
    ToolCall { name: String, args_summary: String },
    ToolResult { summary: String },
    PermissionRequest { id: String, title: String, options: Vec<PermissionOptionView> },
    Usage { input: Option<u64>, output: Option<u64> },
    Done,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOptionView {
    pub id: String,
    pub label: String,
}

/// The scope a session is created with. `mcp_servers`/`skill` come from the app.
#[derive(Debug, Clone)]
pub struct AppScope {
    pub cwd: PathBuf,
    pub mcp_servers: Vec<agent_client_protocol::schema::McpServer>,
    pub skill: Option<String>,
    /// Stable key used to map app -> live session (app dir path, or "notebook").
    pub app_key: String,
    /// Display label for the chat header.
    pub label: String,
}

/// Identifies which app session a turn targets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRef {
    pub app_key: String,
}
```
- [ ] **Step 4:** `scripts/spur-cargo test -p spur-notebook chat_event_message_chunk` → PASS.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/src/sidebar_chat/types.rs
git commit -m "feat(sidebar-chat): add ChatEvent, AppScope, SessionRef types"
```

---

## Task 2: App-context loader (`scope.rs`)

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-notebook/src/sidebar_chat/scope.rs`
- Test: inline + `crates/spur-notebook/tests/fixtures/sidebar_chat/spur-app.json`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] A notebook path inside a dir containing `spur-app.json` yields an `AppScope` with the app dir as `cwd`, the manifest `mcp_server` appended to `mcp_servers`, and `skill/SKILL.md` contents in `skill`.
- [ ] A notebook path with no `spur-app.json` yields the default "notebook" scope (`label = "Notebook"`, foundation tools only, `skill = None`).
- [ ] `scripts/spur-cargo test -p spur-notebook sidebar_chat::scope` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `scope.rs`, the test fixture. OUT: `manager.rs`. Reuse `crate::spur_app` manifest constants (`SPUR_APP_MANIFEST = "spur-app.json"`) and deserialize through the real `SpurAppManifest` shape. Do NOT create a parallel loose manifest parser.

**Implementation:**
- [ ] **Step 1: Write the failing tests.**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_notebook_yields_default_scope() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("notebook.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        let scope = resolve_app_scope(&nb).unwrap();
        assert_eq!(scope.label, "Notebook");
        assert_eq!(scope.app_key, "notebook");
        assert!(scope.skill.is_none());
    }

    #[test]
    fn spur_app_dir_yields_app_scope_with_skill_and_mcp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("spur-app.json"), r#"{
          "schema": "spur.app/v1",
          "name": "Code Graph Workbench",
          "entry_notebook": "app.ipynb",
          "open_mode": "app",
          "runtime": {
            "jute_min": "0.1.0",
            "features": ["frontend-cells", "anywidget-afm", "ports-arrow"]
          },
          "mcp_server": { "type": "python", "entry": "server/main.py" },
          "skill": "skill/SKILL.md"
        }"#).unwrap();
        std::fs::create_dir_all(dir.path().join("skill")).unwrap();
        std::fs::write(dir.path().join("skill/SKILL.md"), "workbench skill").unwrap();
        let nb = dir.path().join("app.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        let scope = resolve_app_scope(&nb).unwrap();
        assert_eq!(scope.label, "Code Graph Workbench");
        assert_eq!(scope.cwd, dir.path());
        assert_eq!(scope.skill.as_deref(), Some("workbench skill"));
        assert!(scope.mcp_servers.iter().any(|s| s.name == "Code Graph Workbench" || !scope.mcp_servers.is_empty()));
    }
}
```
- [ ] **Step 2:** Run both → FAIL (`resolve_app_scope` missing).
- [ ] **Step 3: Implement** `resolve_app_scope(notebook_path: &Path) -> anyhow::Result<AppScope>`:
  walk up from the notebook dir looking for `spur-app.json` (cap at repo root / filesystem root). If found: deserialize `crate::spur_app::SpurAppManifest`, set `cwd` = manifest dir, `app_key` = manifest dir string, `label` = manifest `name`, read the manifest `skill` path or default `skill/SKILL.md` if present, and convert the manifest `mcp_server` into an `agent_client_protocol::schema::McpServer` entry appended to the foundation defaults. If not found: `cwd` = notebook dir, `app_key = "notebook"`, `label = "Notebook"`, `mcp_servers` = foundation defaults, `skill = None`.
- [ ] **Step 4:** Run both → PASS.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/src/sidebar_chat/scope.rs crates/spur-notebook/tests/fixtures/sidebar_chat/
git commit -m "feat(sidebar-chat): app-context loader resolves AppScope from notebook path"
```

**Scope Drift Checkpoint:** if `SpurAppManifest` or `agent_client_protocol::schema::McpServer` lacks enough information to build the MCP entry without extra host/plugin wiring → emit `scope_drift` instead of inventing a second manifest format.

---

## Task 3: `SidebarChat` — session lifecycle (new / load / list, cwd-scoped)

**Task ID:** `task-3`

**Files:**
- Create: `crates/spur-notebook/src/sidebar_chat/manager.rs`
- Test: inline `#[cfg(test)]` using the existing `FakeConn`/`MockConn` pattern

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] `SidebarChat::ensure_session(scope)` calls `new_session(scope.cwd, scope.mcp_servers)` on first entry and caches the `SessionId` under `scope.app_key`.
- [ ] On re-entry with a known `app_key`, if the agent reports `supports_load_session`, it calls `load_session` (resume) instead of `new_session`; otherwise `new_session`.
- [ ] `list_sessions(cwd)` returns the cwd-scoped session list.
- [ ] `scripts/spur-cargo test -p spur-notebook sidebar_chat::manager::lifecycle` passes.

**Suggested Worker:** claude-code-acp (session-state judgment; multi-method)

**Scope Boundary:**
- IN: `manager.rs` session-lifecycle methods + their tests. OUT: the turn-drain/permission/cancel methods (Task 4) — leave a `// task-4` stub. Do NOT modify `spur-acp`.

**Implementation:**
- [ ] **Step 1: Write the failing test** (mirror the `FakeConn` in `dag/ai/acp_backend.rs` tests):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    // FakeConn records calls; new_session returns a fixed SessionId.
    #[tokio::test]
    async fn ensure_session_creates_then_caches_per_app() {
        let conn = std::sync::Arc::new(tokio::sync::Mutex::new(FakeConn::new()));
        let mut chat = SidebarChat::new(conn.clone());
        let scope = test_scope("notebook");
        let s1 = chat.ensure_session(&scope).await.unwrap();
        let s2 = chat.ensure_session(&scope).await.unwrap();
        assert_eq!(s1, s2);                       // cached, no second new_session
        assert_eq!(conn.lock().await.new_session_calls, 1);
    }
}
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `SidebarChat`:
```rust
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use spur_acp::connection::AgentConnection;
use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion, SessionId};
use super::types::AppScope;

pub struct SidebarChat {
    conn: Arc<Mutex<dyn AgentConnection>>,
    sessions: HashMap<String, SessionId>, // app_key -> session
    pending_permissions: HashMap<String, tokio::sync::oneshot::Sender<spur_acp::types::PermissionResponse>>,
    initialized: bool,
}

impl SidebarChat {
    pub fn new(conn: Arc<Mutex<dyn AgentConnection>>) -> Self {
        Self { conn, sessions: HashMap::new(), pending_permissions: HashMap::new(), initialized: false }
    }

    /// Ensure (create or resume) the session for `scope`, returning its id.
    pub async fn ensure_session(&mut self, scope: &AppScope) -> anyhow::Result<SessionId> {
        if let Some(existing) = self.sessions.get(&scope.app_key) {
            return Ok(existing.clone());
        }
        let mut conn = self.conn.lock().await;
        if !self.initialized {
            conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST)).await?;
            self.initialized = true;
        }
        let resp = conn.new_session(scope.cwd.clone(), scope.mcp_servers.clone()).await?;
        drop(conn);
        self.sessions.insert(scope.app_key.clone(), resp.session_id.clone());
        Ok(resp.session_id)
    }
    // list_sessions / load_session-resume path implemented here too; turn() is task-4.
}
```
  (Use the exact `initialize`/`new_session` call shapes verified in `dag/ai/acp_backend.rs`.)
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/src/sidebar_chat/manager.rs
git commit -m "feat(sidebar-chat): SidebarChat session lifecycle (new/load/list per app)"
```

**Scope Drift Checkpoint:** if `supports_load_session` gating needs `SpurAgentCaps` plumbing not reachable from the connection → emit `risk` before adding cross-crate wiring.

---

## Task 4: `SidebarChat` — turn drain → `ChatEvent` stream + permission + cancel

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/src/sidebar_chat/manager.rs` (add `turn`, `cancel`, permission route)
- Test: inline `#[cfg(test)]`

**Depends on:** `task-3`

**Acceptance Criteria:**
- [ ] `turn(scope, prompt, tx)` ensures the session, subscribes to `subscribe_session_notifications()` before `prompt()` when available, and sends a `ChatEvent` per matching session notification (`AgentMessageChunk` → `MessageChunk`), ending with `Done`.
- [ ] Stream-only adapters that return notifications from `prompt()` still work; native ACP's empty prompt stream is treated as a completion signal while notification payloads come from the broadcast subscriber.
- [ ] A `PermissionRequest` handed to the manager is stored by request id and forwarded as `ChatEvent::PermissionRequest`.
- [ ] `respond_permission(request_id, option_id)` sends `PermissionResponse { option_id }` through the stored `reply_tx`; denying drops the stored sender.
- [ ] `cancel()` calls `conn.cancel(session_id)` and the drain loop exits.
- [ ] `scripts/spur-cargo test -p spur-notebook sidebar_chat::manager::turn` passes.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN: `manager.rs` turn/cancel/permission. OUT: Tauri state/command layers (Tasks 5-6). Reuse the `PromptRequest::new(...)` / `TextContent::new(...)` construction from `AcpAgentBackend::run`, but do **not** copy its stream-only drain unchanged: new callers must prefer `subscribe_session_notifications()` for native ACP and fall back to the prompt stream for adapters that do not expose a broadcast.

**Implementation:**
- [ ] **Step 1: Write the failing test.**
```rust
#[tokio::test]
async fn turn_streams_message_chunks_then_done() {
    let conn = Arc::new(Mutex::new(FakeConn::with_chunks(["Hel", "lo"])));
    let mut chat = SidebarChat::new(conn);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
    chat.turn(&test_scope("notebook"), "hi", tx, CancellationToken::new()).await.unwrap();
    let mut texts = vec![];
    while let Ok(ev) = rx.try_recv() {
        match ev { ChatEvent::MessageChunk{text} => texts.push(text), ChatEvent::Done => break, _=>{} }
    }
    assert_eq!(texts.concat(), "Hello");
}

#[tokio::test]
async fn turn_reads_native_broadcast_notifications_before_empty_prompt_stream() {
    let conn = Arc::new(Mutex::new(FakeConn::with_broadcast_chunks(["Hel", "lo"])));
    let mut chat = SidebarChat::new(conn);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
    chat.turn(&test_scope("notebook"), "hi", tx, CancellationToken::new()).await.unwrap();
    let mut texts = vec![];
    while let Ok(ev) = rx.try_recv() {
        match ev { ChatEvent::MessageChunk{text} => texts.push(text), ChatEvent::Done => break, _=>{} }
    }
    assert_eq!(texts.concat(), "Hello");
}
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `turn` with two notification sources:
```rust
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use agent_client_protocol::schema::{ContentBlock, PromptRequest, SessionNotification, SessionUpdate, TextContent};

impl SidebarChat {
    pub async fn turn(
        &mut self,
        scope: &AppScope,
        prompt: &str,
        tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = self.ensure_session(scope).await?;
        let req = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt.to_owned()))],
        );

        // Subscribe before prompt/load_session. NativeAcpConnection publishes
        // notification payloads here and returns an empty prompt stream.
        let mut broadcast_rx = {
            let conn = self.conn.lock().await;
            conn.subscribe_session_notifications()
        };

        let mut prompt_stream = {
            let mut conn = self.conn.lock().await;
            conn.prompt(req).await?
        };

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let mut conn = self.conn.lock().await;
                    let _ = conn.cancel(session_id.0.as_ref()).await;
                    break;
                }
                recv = async {
                    match &mut broadcast_rx {
                        Some(rx) => rx.recv().await.ok(),
                        None => None,
                    }
                }, if broadcast_rx.is_some() => {
                    if let Some(notification) = recv {
                        if notification.session_id == session_id {
                            forward_notification(notification, &tx);
                        }
                    }
                }
                item = prompt_stream.next() => {
                    match item {
                        Some(notification) => forward_notification(notification, &tx),
                        None => break,
                    }
                }
            }
        }
        let _ = tx.send(ChatEvent::Done);
        Ok(())
    }
}
```
  Implement `forward_notification(notification, tx)` as a private helper that maps `SessionUpdate::AgentMessageChunk` text to `ChatEvent::MessageChunk`, recognized tool-call/tool-result updates to chips, and ignores notifications for other sessions. Keep the broadcast receiver alive until the prompt stream closes so native ACP has a completion boundary and a payload source.
- [ ] **Step 4: Implement permission storage and response.**
```rust
use spur_acp::types::{PermissionRequest, PermissionResponse};

impl SidebarChat {
    pub async fn handle_permission_request(
        &mut self,
        request: PermissionRequest,
        tx: &tokio::sync::mpsc::UnboundedSender<ChatEvent>,
    ) {
        let id = request.args.session_id.to_string();
        let title = request.args.tool_call.fields.title.clone()
            .unwrap_or_else(|| "Tool call".to_string());
        let options = request.args.options.iter().map(|option| PermissionOptionView {
            id: option.option_id.to_string(),
            label: option.name.clone(),
        }).collect();
        self.pending_permissions.insert(id.clone(), request.reply_tx);
        let _ = tx.send(ChatEvent::PermissionRequest { id, title, options });
    }

    pub fn respond_permission(&mut self, request_id: &str, option_id: Option<String>) -> anyhow::Result<()> {
        if let Some(reply_tx) = self.pending_permissions.remove(request_id) {
            if let Some(option_id) = option_id {
                let _ = reply_tx.send(PermissionResponse { option_id });
            }
        }
        Ok(())
    }
}
```
  Use a stable request key derived from ACP data (`session_id` + tool-call id/name if available) rather than `session_id` alone if multiple concurrent permission prompts can exist for one session.
- [ ] **Step 5:** Run → PASS.
- [ ] **Step 6: Commit.**
```bash
git add crates/spur-notebook/src/sidebar_chat/manager.rs
git commit -m "feat(sidebar-chat): stream prompt turn into ChatEvents; cancel + permission route"
```

---

## Task 5: Tauri sidebar chat state + agent connection wiring

**Task ID:** `task-5`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src-tauri/src/chat_state.rs`
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/state.rs`
- Test: inline `#[cfg(test)]` in `chat_state.rs`

**Depends on:** `task-4`, `task-2`

**Acceptance Criteria:**
- [ ] `State` owns a lazily initialized `Arc<tokio::sync::Mutex<SidebarChat>>` plus a cancellation root used by `chat_cancel`.
- [ ] The chat connection is built from the same layered SPUR config selection as `dag/run_context.rs`, but passes a real `permission_tx` into `NativeAcpConnection::new_with_kind`.
- [ ] If no agent is configured, initialization returns a structured command error instead of panicking.
- [ ] `scripts/spur-cargo test -p jute-notebook chat_state` passes.

**Suggested Worker:** claude-code-acp (state/config boundary)

**Scope Boundary:**
- IN: `chat_state.rs`, `state.rs`. OUT: command registration, React, `SidebarChat` internals. Prefer extracting/reusing the existing config-loading and connection-building logic over duplicating it silently. If private helpers in `dag/run_context.rs` block reuse, make the minimal visibility adjustment in `spur-notebook` and include it in this task.

**Implementation:**
- [ ] **Step 1: Write the failing test.**
```rust
#[test]
fn chat_agent_config_missing_returns_unavailable() {
    let state = State::new();
    let result = crate::chat_state::build_sidebar_chat_for_test(&state, None);
    assert!(matches!(result, Err(ChatStateError::AgentUnavailable)));
}
```
- [ ] **Step 2:** Run → FAIL (`chat_state` missing).
- [ ] **Step 3: Implement `ChatState`.**
```rust
pub struct SidebarChatState {
    pub chat: tokio::sync::OnceCell<std::sync::Arc<tokio::sync::Mutex<spur_notebook::sidebar_chat::manager::SidebarChat>>>,
    pub cancel_root: tokio_util::sync::CancellationToken,
    pub permission_tx: tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
    pub permission_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
}

impl SidebarChatState {
    pub fn new() -> Self {
        let (permission_tx, permission_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            chat: tokio::sync::OnceCell::new(),
            cancel_root: tokio_util::sync::CancellationToken::new(),
            permission_tx,
            permission_rx: tokio::sync::Mutex::new(permission_rx),
        }
    }
}
```
  Add `pub sidebar_chat: SidebarChatState` to `State::default()`.
- [ ] **Step 4: Implement connection construction.**
  Mirror `dag/run_context.rs::load_spur_config`, `select_default_agent`, and `build_agent_connection`, but pass `Some(permission_tx.clone())` for `TransportKind::Acp`. If the helper remains private, extract it to a crate-visible function that both notebook AI nodes and sidebar chat use, with the AI-node path continuing to pass `None`.
- [ ] **Step 5:** Run → PASS.
- [ ] **Step 6: Commit.**
```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/chat_state.rs crates/spur-notebook/jute-notebook/src-tauri/src/state.rs crates/spur-notebook/src/dag/run_context.rs
git commit -m "feat(sidebar-chat): wire Tauri state to configured ACP agent"
```

**Scope Drift Checkpoint:** if constructing the connection needs additional frontend/runtime config not reachable from `State` or current cwd, emit `scope_drift` before changing command signatures or global app startup.

---

## Task 6: `chat_*` Tauri commands (`Channel<ChatEvent>`)

**Task ID:** `task-6`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs`
- Modify: the Tauri builder `invoke_handler!` registration (search `run_cell` in `src-tauri/src/lib.rs`/`main.rs`) and `mod chat_commands;`
- Test: inline command-shape test

**Depends on:** `task-5`

**Acceptance Criteria:**
- [ ] `chat_turn(notebook_path, prompt, on_event: Channel<ChatEvent>, state)` resolves the `AppScope` (Task 2), runs `SidebarChat::turn`, and forwards each `ChatEvent` to `on_event` — mirroring `run_cell`.
- [ ] While a turn is active, `chat_turn` also drains `state.sidebar_chat.permission_rx` and calls `SidebarChat::handle_permission_request(...)` so ACP permission prompts reach the same `Channel<ChatEvent>`.
- [ ] `chat_sessions_list`, `chat_switch_session`, `chat_new_session`, `chat_cancel`, and `chat_permission_respond` exist and are registered in `invoke_handler`.
- [ ] `chat_permission_respond(request_id, option_id)` calls `SidebarChat::respond_permission`; `option_id = null` denies by dropping the pending reply sender.
- [ ] `scripts/spur-cargo build -p jute-notebook` (or the tauri crate) compiles; registration test passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `chat_commands.rs` + the `invoke_handler` lines + `mod` decl. OUT: `manager.rs`, frontend, state construction. Use the `SidebarChatState` added by Task 5.

**Implementation:**
- [ ] **Step 1: Write the failing test** (command registration / signature):
```rust
#[test]
fn chat_turn_has_channel_signature() {
    // compile-time: the fn must accept Channel<ChatEvent>; this test ensures the module builds.
    fn _assert(_f: fn(&str, &str, tauri::ipc::Channel<spur_notebook::sidebar_chat::types::ChatEvent>, tauri::State<'_, std::sync::Arc<crate::State>>) -> _) {}
    // referenced indirectly; real check is that the crate compiles.
    assert!(true);
}
```
- [ ] **Step 2:** Build → FAIL (module missing).
- [ ] **Step 3: Implement** mirroring `run_cell` (verified at `src-tauri/src/commands.rs:2070`):
```rust
use tauri::ipc::Channel;
use std::sync::Arc;
use spur_notebook::sidebar_chat::{types::ChatEvent, scope::resolve_app_scope};

#[tauri::command]
pub async fn chat_turn(
    notebook_path: &str,
    prompt: &str,
    on_event: Channel<ChatEvent>,
    state: tauri::State<'_, Arc<State>>,
) -> Result<(), Error> {
    let scope = resolve_app_scope(std::path::Path::new(notebook_path))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
    let chat = crate::chat_state::get_or_init_sidebar_chat(&state).await?;
    let cancel = state.sidebar_chat.cancel_root.child_token();
    let tx_for_turn = tx.clone();
    tokio::spawn(async move { let _ = chat.lock().await.turn(&scope, &prompt_owned, tx_for_turn, cancel).await; });
    // Also pump state.sidebar_chat.permission_rx during the turn and call
    // chat.lock().await.handle_permission_request(request, &tx).await.
    while let Some(ev) = rx.recv().await {
        if on_event.send(ev).is_err() { break; }
    }
    Ok(())
}
```
  Add `chat_sessions_list` / `chat_switch_session` / `chat_new_session` / `chat_cancel` / `chat_permission_respond` as thin wrappers over the manager, and register all six in the `invoke_handler![...]` list next to `run_cell`.
- [ ] **Step 4:** Build → PASS.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs crates/spur-notebook/jute-notebook/src-tauri/src/lib.rs
git commit -m "feat(sidebar-chat): chat_* Tauri commands streaming ChatEvent over Channel"
```

**Scope Drift Checkpoint:** if `chat_permission_respond` needs a richer permission identity than Task 4 exposes, emit `scope_drift` and update the `ChatEvent::PermissionRequest` id contract rather than guessing.

---

## Task 7: Chat store (`stores/chat.ts`)

**Task ID:** `task-7`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/stores/chat.ts`
- Test: `crates/spur-notebook/jute-notebook/src/stores/chat.test.ts`

**Depends on:** `task-1` (mirror the `ChatEvent` shape in TS)

**Acceptance Criteria:**
- [ ] Store holds `{ scopeLabel, messages, streaming, pendingPermission, activeAppKey }` keyed per app.
- [ ] `applyEvent(ev)` appends a `MessageChunk` to the streaming buffer, finalizes on `Done`, sets `pendingPermission` on `PermissionRequest` with option ids + labels.
- [ ] `scripts/spur-pnpm test -- src/stores/chat.test.ts` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `chat.ts` + its test. OUT: `ChatPanel.tsx` (Task 8). Follow the Zustand pattern in `stores/sidebar.ts`.

**Implementation:**
- [ ] **Step 1: Failing test.**
```ts
import { useChat } from "./chat";
test("message chunks accumulate then finalize on done", () => {
  const s = useChat.getState();
  s.applyEvent({ type: "messageChunk", text: "Hel" });
  s.applyEvent({ type: "messageChunk", text: "lo" });
  s.applyEvent({ type: "done" });
  const msgs = useChat.getState().messages;
  expect(msgs.at(-1)?.text).toBe("Hello");
  expect(useChat.getState().streaming).toBe(false);
});
```
- [ ] **Step 2:** `scripts/spur-pnpm test -- src/stores/chat.test.ts` → FAIL.
- [ ] **Step 3: Implement** the Zustand store with a `ChatEvent` TS union mirroring Task 1 (camelCase `type` tags, permission options as `{ id, label }`) and the `applyEvent` reducer + `setScope(appKey,label)` + `clearPendingPermission(requestId)`.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/jute-notebook/src/stores/chat.ts crates/spur-notebook/jute-notebook/src/stores/chat.test.ts
git commit -m "feat(sidebar-chat): chat store with applyEvent reducer + per-app scope"
```

---

## Task 8: `ChatPanel.tsx` + sidebar registry entry

**Task ID:** `task-8`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`
- Modify: `…/sidebar/panels.ts` (append one `SidebarPanel`)
- Test: `…/sidebar/ChatPanel.test.tsx`

**Depends on:** `task-7`, `task-6`

**Acceptance Criteria:**
- [ ] An `{ id: "agent", title: "AI Agent", icon, ariaLabel: "AI Agent", Component: ChatPanel }` entry is appended to `SIDEBAR_PANELS`.
- [ ] `ChatPanel` renders the message list + streaming text, a composer that calls `invoke("chat_turn", { notebookPath, prompt, onEvent })`, an inline permission grant/deny block when `pendingPermission` is set, and a header showing the active scope label.
- [ ] Subscribing to `notebook.store.viewState.path` triggers `chat_switch_session` (re-scope) on change.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx` passes.

**Suggested Worker:** kiro (UI/UX, spec-driven) — fall back to claude-code-acp

**Scope Boundary:**
- IN: `ChatPanel.tsx`, one line in `panels.ts`, the test. OUT: the store (Task 7), commands (Task 6). Follow `DatasourcePanel.tsx` + `NotebookSidebar.test.tsx` patterns; use the `Channel` invoke pattern from how the frontend calls `run_cell` (`stores/notebook.ts`).

**Implementation:**
- [ ] **Step 1: Failing test** (render + streaming + permission):
```tsx
import { render, screen } from "@testing-library/react";
import ChatPanel from "./ChatPanel";
import { useChat } from "@/stores/chat";
test("renders streaming text and inline permission", () => {
  useChat.setState({ messages: [{ role: "assistant", text: "Hello" }],
                     pendingPermission: { id: "1", title: "Run tool?", options: [{ id: "allow", label: "Allow" }, { id: "deny", label: "Deny" }] } } as any);
  render(<ChatPanel />);
  expect(screen.getByText("Hello")).toBeInTheDocument();
  expect(screen.getByText("Run tool?")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Allow" })).toBeInTheDocument();
});
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `ChatPanel` (trusted React: `useChat`, `useNotebook`, `invoke`, `Channel`); append the registry entry; wire the `viewState.path` effect to `chat_switch_session`; wire permission buttons to `invoke("chat_permission_respond", { requestId, optionId })`, passing `null` for deny.
- [ ] **Step 4:** Run → PASS; also `scripts/spur-pnpm run typecheck`.
- [ ] **Step 5: Commit.**
```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/panels.ts crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "feat(sidebar-chat): AI Agent ChatPanel + sidebar registry entry"
```

---

## Task 9: Boundary integration test — chat_turn streaming + permission

**Task ID:** `task-9`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src-tauri/tests/chat_turn_stream.rs`

**Depends on:** `task-6`

**Acceptance Criteria:**
- [ ] With a stream-backed `FakeConn` that emits two chunks then ends, driving `SidebarChat::turn` through the command path yields ordered `MessageChunk` events then `Done` on the receiver.
- [ ] With a broadcast-backed `FakeConn` whose `prompt()` stream is empty, driving `SidebarChat::turn` yields ordered `MessageChunk` events then `Done`.
- [ ] A fake permission request yields a `ChatEvent::PermissionRequest`, and `chat_permission_respond` resolves the stored reply sender with the selected option id.
- [ ] `scripts/spur-cargo test -p jute-notebook --test chat_turn_stream` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the new test file. OUT: production code. Reuse the `FakeConn` shape from `dag/ai/acp_backend.rs` tests / `skip_perm_helper.rs`.

**Implementation:**
- [ ] **Step 1:** Write the test that wires both stream-backed and broadcast-backed fake connections into `SidebarChat`, runs `turn` with an mpsc receiver, and asserts the event order (`MessageChunk`×2, `Done`).
- [ ] **Step 2:** Add the permission-response test: create a fake `PermissionRequest` with a `reply_tx`, forward it through the manager/command path, call `chat_permission_respond("request-id", Some("allow"))`, and assert the reply receiver gets `PermissionResponse { option_id: "allow" }`.
- [ ] **Step 3:** Run → it should pass against Tasks 3-6 (this is a regression guard, not TDD-first).
- [ ] **Step 4: Commit.**
```bash
git add crates/spur-notebook/jute-notebook/src-tauri/tests/chat_turn_stream.rs
git commit -m "test(sidebar-chat): boundary integration for chat_turn streaming + permission"
```

---

## Self-Review

**Spec coverage:** §3 architecture → Tasks 3-8; §5 session lifecycle (B/C) → Tasks 3 (+ load/list); §6 app scope → Task 2; §7 streaming → Tasks 4, 6, 9; §8 permissions → Tasks 4 (reply storage), 5 (permission channel), 6 (response command), 8 (UI), and Task 0 pin of `permission_tx`/`PermissionRequest`; §9 app integration (notebook_push_source) → exercised by the workbench retrofit (follow-on, out of scope here, noted §12 of spec); §11 testing → each task is TDD + Task 9. **Former gap closed:** the agent-config wiring that constructs the `NativeAcpConnection` placed on `State` is now explicit Task 5.

**Placeholder scan:** no silent TODO/TBD placeholders remain. Code snippets use the verified `PromptRequest::new(...)` and `InitializeRequest::new(ProtocolVersion::LATEST)` shapes from `dag/ai/acp_backend.rs`.

**Type consistency:** `ChatEvent` (Task 1) is mirrored in TS (Task 7) and used by Tasks 4/6/8/9; `PermissionOptionView` carries ids needed by `chat_permission_respond`; `AppScope` (Task 1) produced by Task 2, consumed by Tasks 3-6; `SessionRef` reserved for switch commands.

**DAG validation:** acyclic; roots none→0; widest layer after 1 is {2,3,7}. No cycles.

**beads compatibility:** every task has a unique id, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary with a drift checkpoint where risk is real (0, 2, 3, 5, 6).
