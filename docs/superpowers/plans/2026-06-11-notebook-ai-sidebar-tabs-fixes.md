# Notebook AI Sidebar Multi-Tab Fixes Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-09-notebook-sidebar-ai-agent-design.md`
**Design epic:** Review-derived follow-up from the 2026-06-11 AI sidebar + notebook-tabs integration review.

**Goal:** Fix AI sidebar behavior across mounted notebook tabs and Spur App mode so chat scope, ACP sessions, streamed events, and resume UI follow the user-visible notebook/app.

**Architecture:** Treat chat scope as an explicit key, not implicit global UI state. Plain notebooks get per-notebook keys, Spur Apps keep app-root keys, frontend events route to the originating scope, and app mode still renders the shared sidebar agent.

**Tech Stack:** Rust 2021, Tauri commands, React, Zustand, Vitest, Testing Library.

---

### Task 1: Backend Plain-Notebook Chat Scope Keys

**Task ID:** `task-backend-notebook-chat-scope`

**Files:**
- Modify: `crates/spur-notebook/src/sidebar_chat/scope.rs`
- Modify: `crates/spur-notebook/src/sidebar_chat/types.rs`
- Modify: `crates/spur-notebook/src/sidebar_chat/manager.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Plain notebooks in different files receive distinct `AppScope.app_key` values.
- [ ] Plain notebook `cwd` remains the notebook directory.
- [ ] Spur App scopes still use the app root as `app_key`.
- [ ] `SidebarChat::ensure_session` creates separate ACP sessions for two plain notebooks with different app keys.
- [ ] `scripts/spur-cargo test -p spur-notebook sidebar_chat --lib` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: sidebar chat scope construction, AppScope comments/tests, manager cache tests.
- OUT of scope: frontend `ChatPanel`, Tauri command signatures, ACP protocol changes.
- If changing command payloads becomes necessary, emit `scope_drift`.

**Implementation:**
- [ ] Add a failing Rust test in `scope.rs`:

```rust
#[test]
fn plain_notebooks_get_distinct_scope_keys() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.ipynb");
    let second = dir.path().join("second.ipynb");
    std::fs::write(&first, "{}").unwrap();
    std::fs::write(&second, "{}").unwrap();

    let first_scope = resolve_app_scope(&first).unwrap();
    let second_scope = resolve_app_scope(&second).unwrap();

    assert_eq!(first_scope.cwd, dir.path());
    assert_eq!(second_scope.cwd, dir.path());
    assert_ne!(first_scope.app_key, second_scope.app_key);
    assert!(first_scope.app_key.contains("first.ipynb"));
    assert!(second_scope.app_key.contains("second.ipynb"));
}
```

- [ ] Run the focused test and confirm it fails:

```bash
scripts/spur-cargo test -p spur-notebook plain_notebooks_get_distinct_scope_keys --lib
```

- [ ] Change `default_notebook_scope` to receive the notebook path and set `app_key` to a stable plain-notebook key such as `notebook:<path>`, while keeping `label = "Notebook"` and `cwd = parent_dir`.
- [ ] Update the existing `plain_notebook_yields_default_scope` test so it asserts the new per-notebook key shape instead of the shared literal `"notebook"`.
- [ ] Add or update a manager test so two scopes with different plain-notebook keys create two sessions even if their `cwd` is the same.
- [ ] Run:

```bash
scripts/spur-cargo test -p spur-notebook sidebar_chat --lib
```

- [ ] Commit:

```bash
git add crates/spur-notebook/src/sidebar_chat/scope.rs crates/spur-notebook/src/sidebar_chat/types.rs crates/spur-notebook/src/sidebar_chat/manager.rs
git commit -m "fix(spur-notebook): chat scope sessions per notebook"
```

### Task 2: Frontend Scoped Chat Store And Event Routing

**Task ID:** `task-frontend-chat-scope-routing`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/chat.ts`
- Modify: `crates/spur-notebook/jute-notebook/src/stores/chat.test.ts`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx`

**Depends on:** `task-backend-notebook-chat-scope`

**Acceptance Criteria:**
- [ ] `ChatPanel` computes the same scope key contract as the backend: app root for Spur Apps, `notebook:<path>` for plain notebooks, default only when no path exists.
- [ ] Streamed `ChatEvent`s are applied to the scope that started the turn, not the store's current active scope at delivery time.
- [ ] Hidden mounted tab panels cannot move streamed text/errors/permissions into the visible tab's conversation.
- [ ] Existing singleton chat behavior remains intact.
- [ ] `scripts/spur-pnpm test -- src/stores/chat.test.ts src/ui/notebook/sidebar/ChatPanel.test.tsx` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: chat Zustand store API, ChatPanel scope key calculation, ChatPanel unit tests.
- OUT of scope: `NotebookPage` tab architecture, Tauri command implementation, backend Rust session manager.
- If a larger provider/context refactor appears necessary, emit `scope_drift`.

**Implementation:**
- [ ] Add failing store tests that prove events can be routed to a non-active scope:

```ts
test("applies events to the supplied scope key instead of the active scope", () => {
  const s = useChat.getState();
  s.setScope("notebook:/tmp/a.ipynb", "a.ipynb");
  s.setScope("notebook:/tmp/b.ipynb", "b.ipynb");

  s.applyEventForScope("notebook:/tmp/a.ipynb", {
    type: "messageChunk",
    text: "A",
  });
  s.applyEventForScope("notebook:/tmp/a.ipynb", { type: "done" });

  expect(useChat.getState().conversations["notebook:/tmp/a.ipynb"].messages.map((m) => m.text)).toEqual(["A"]);
  expect(useChat.getState().messages.map((m) => m.text)).toEqual([]);
});
```

- [ ] Add a failing `ChatPanel.test.tsx` case that mounts two panels by changing the mocked notebook path between renders, starts a turn from the first scope, switches scope before channel delivery, and asserts the chunk remains in the first scope.
- [ ] Extend `ChatActions` with scoped operations:

```ts
applyEventForScope: (appKey: string, event: ChatEvent) => void;
clearPendingPermissionForScope: (appKey: string, requestId: string) => void;
```

- [ ] Keep existing `applyEvent` and `clearPendingPermission` as compatibility wrappers that target `state.activeAppKey`.
- [ ] In `ChatPanel`, derive:

```ts
const chatScopeKey = appOpenInfo?.app_root
  ?? (notebookPath ? `notebook:${notebookPath}` : DEFAULT_CHAT_APP_KEY);
```

- [ ] Select the panel's own conversation from `state.conversations[chatScopeKey]` instead of relying on top-level projected `messages`.
- [ ] Use `applyEventForScope(chatScopeKey, message)` in the Tauri `Channel` callback and in catch handlers for the originating turn.
- [ ] Use `clearPendingPermissionForScope(chatScopeKey, requestId)` after permission responses.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/stores/chat.test.ts src/ui/notebook/sidebar/ChatPanel.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/stores/chat.ts crates/spur-notebook/jute-notebook/src/stores/chat.test.ts crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "fix(spur-notebook): route sidebar chat by scope"
```

### Task 3: Render AI Sidebar In Spur App Mode

**Task ID:** `task-app-mode-sidebar-agent`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookView.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/pages/NotebookPage.test.tsx` or create a focused `NotebookView` test beside the component.

**Depends on:** `task-frontend-chat-scope-routing`

**Acceptance Criteria:**
- [ ] App mode still hides notebook chrome that should remain hidden: location, footer, command menu, and HTML notice.
- [ ] App mode renders `NotebookSidebar`, including the AI Agent panel entry.
- [ ] Non-app modes keep the existing sidebar layout.
- [ ] `scripts/spur-pnpm test -- src/pages/NotebookPage.test.tsx` plus any new `NotebookView` test passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `NotebookView` layout and tests around app-mode sidebar visibility.
- OUT of scope: AppMode internals, sidebar panel styling beyond width/grid compatibility.
- If app canvases need a product design change to fit beside the sidebar, emit `risk`.

**Implementation:**
- [ ] Add a failing test that renders a notebook in `viewMode: "app"` and asserts the sidebar toggle/AI Agent control is present while footer and command menu remain absent.
- [ ] Change `NotebookView` so app mode uses the sidebar grid column and renders `<NotebookSidebar />`; keep notebook location/footer/command menu suppressed in app mode.
- [ ] Verify the app content container still uses `overflow-hidden` for app mode.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/pages/NotebookPage.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookView.tsx crates/spur-notebook/jute-notebook/src/pages/NotebookPage.test.tsx
git commit -m "fix(spur-notebook): show AI sidebar in app mode"
```

### Task 4: Add Session List And Resume UI To ChatPanel

**Task ID:** `task-chat-session-picker`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx`

**Depends on:** `task-frontend-chat-scope-routing`

**Acceptance Criteria:**
- [ ] `ChatPanel` calls `chat_sessions_list` for the current notebook/app scope when a saved notebook path is available.
- [ ] The panel exposes a compact session selector when sessions are returned.
- [ ] Selecting a session invokes `chat_switch_session` with the current `notebookPath` and selected `sessionId`.
- [ ] Starting a new session still uses `chat_new_session` and refreshes the selector state.
- [ ] Session-list failures are rendered as chat errors scoped to the current panel.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: ChatPanel session-list state, minimal selector UI, existing Tauri command calls.
- OUT of scope: backend session persistence, transcript replay rendering, ACP capability detection UI.
- If `SessionInfo` bindings are missing or inaccurate, emit `scope_drift` before editing generated bindings.

**Implementation:**
- [ ] Add failing tests that mock `chat_sessions_list` returning two sessions, assert a selector is shown, select one session, and verify `chat_switch_session` receives the chosen id.
- [ ] Add a small local `SessionInfo` type in `ChatPanel.tsx` if generated bindings are not already available:

```ts
type SessionInfo = {
  id: string;
  cwd?: string | null;
};
```

- [ ] On scope change, call `chat_sessions_list` after `setScope`; if it returns sessions, preserve them in component state.
- [ ] Keep existing behavior that ensures a session exists, but update the local selected session id from the `chat_new_session` result.
- [ ] Render a native `<select aria-label="Agent session">` with session ids or short labels; keep it visually compact in the existing header area.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "feat(spur-notebook): add sidebar chat session picker"
```

---

## Dependency DAG

```text
task-backend-notebook-chat-scope
  -> task-frontend-chat-scope-routing
       -> task-app-mode-sidebar-agent
       -> task-chat-session-picker
```

## Final Verification

After all tasks are approved and merged by the plan engine, run:

```bash
scripts/spur-cargo test -p spur-notebook sidebar_chat --lib
scripts/spur-pnpm test -- src/stores/chat.test.ts src/ui/notebook/sidebar/ChatPanel.test.tsx src/pages/NotebookPage.test.tsx
scripts/spur-pnpm run typecheck
```

If the remote frontend runner fails before test execution with infrastructure errors such as `ENOTDIR` while creating `node_modules`, report that as a blocker with the full command and stderr rather than claiming tests passed.
