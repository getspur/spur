# AI Sidebar Single-Tab Chat Panel Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-ai-sidebar-single-tab-chat-panel-design.md`
**Design epic:** `01e535ff3` (approved spec commit)

**Goal:** Polish the existing AI Agent sidebar chat panel for the approved single-tab UX direction.

**Architecture:** Keep the existing `ChatPanel` data flow, store contract, and Tauri command payloads. Add tests for the approved hierarchy and copy first, then restructure only the `ChatPanel.tsx` markup/classes so scope, transcript, permission, tool activity, and composer states scan correctly inside the existing 320px sidebar.

**Tech Stack:** React, TypeScript, Zustand, Tauri `invoke`, Testing Library, Vitest, Tailwind utility classes.

---

### Task 1: Add Single-Tab Chat Panel UX Tests

**Task ID:** `task-chat-panel-ux-tests`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Tests assert the approved single-tab empty state copy for plain notebooks.
- [ ] Tests assert the approved app-scope empty state copy when `appOpenInfo` is present.
- [ ] Tests assert the header/composer status makes the active scope obvious.
- [ ] Tests assert tool calls/results render as timeline-style events using `Tool call: <name>` and `Tool result` text.
- [ ] Tests assert pending permission changes the visible status to `Waiting for permission`.
- [ ] Focused test command fails before implementation for the new expectations.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `ChatPanel.test.tsx` test additions/adjustments only.
- OUT of scope: `ChatPanel.tsx`, store behavior, backend commands, snapshot tests, multi-tab behavior.
- If implementation changes appear necessary in this task, stop after committing the failing tests and leave implementation to `task-chat-panel-ux-implementation`.

**Implementation:**
- [ ] Add a failing test for plain-notebook header, empty state, and composer status:

```tsx
test("renders approved single-tab notebook empty state and scoped composer status", async () => {
  render(<ChatPanel />);

  expect(await screen.findByText("revenue.ipynb")).toBeInTheDocument();
  expect(screen.getByText("Active scope")).toBeInTheDocument();
  expect(screen.getByText("Ready with scoped tools enabled")).toBeInTheDocument();
  expect(screen.getByText("Ready in revenue.ipynb")).toBeInTheDocument();
  expect(screen.getByText("Ask inside this notebook")).toBeInTheDocument();
  expect(
    screen.getByText("The assistant can inspect cells, draft edits, and explain outputs."),
  ).toBeInTheDocument();
});
```

- [ ] Add a failing test for app-scope copy:

```tsx
test("renders approved app-scope empty state copy", async () => {
  appOpenInfo = {
    app_name: "Code Graph Workbench",
    app_root: "/tmp/apps/code-graph-workbench",
  };

  render(<ChatPanel />);

  expect(await screen.findByText("Code Graph Workbench")).toBeInTheDocument();
  expect(screen.getByText("/tmp/apps/code-graph-workbench")).toBeInTheDocument();
  expect(screen.getByText("Ask inside this app context")).toBeInTheDocument();
  expect(
    screen.getByText(
      "The assistant can inspect notebook cells, call app tools, and update panels.",
    ),
  ).toBeInTheDocument();
});
```

- [ ] Add a failing test for timeline-style tool event labels and permission status:

```tsx
test("renders tool events as timeline rows and surfaces permission status", async () => {
  const s = useChat.getState();
  s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
  s.applyEventForScope("notebook:/tmp/revenue.ipynb", {
    type: "toolCall",
    name: "code_symbol_search",
    argsSummary: "query=ChatPanel",
  });
  s.applyEventForScope("notebook:/tmp/revenue.ipynb", {
    type: "toolResult",
    summary: "1 symbol found",
  });
  s.applyEventForScope("notebook:/tmp/revenue.ipynb", {
    type: "permissionRequest",
    id: "perm-1",
    title: "Run notebook edit?",
    options: [
      { id: "allow", label: "Allow" },
      { id: "deny", label: "Deny" },
    ],
  });

  render(<ChatPanel />);

  expect(await screen.findByText("Tool call: code_symbol_search")).toBeInTheDocument();
  expect(screen.getByText("query=ChatPanel")).toBeInTheDocument();
  expect(screen.getByText("Tool result")).toBeInTheDocument();
  expect(screen.getByText("1 symbol found")).toBeInTheDocument();
  expect(screen.getByText("Waiting for permission")).toBeInTheDocument();
});
```

- [ ] Run the focused test command and confirm the new expectations fail:

```bash
scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx
```

- [ ] Commit only the test file:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "test(spur-notebook): cover ai sidebar single-tab ux"
```

### Task 2: Implement Single-Tab Chat Panel UI Polish

**Task ID:** `task-chat-panel-ux-implementation`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`

**Depends on:** `task-chat-panel-ux-tests`

**Acceptance Criteria:**
- [ ] Active scope is the most prominent header content.
- [ ] Header shows app root when app-scoped and notebook path hint when notebook-scoped.
- [ ] Status strip reflects ready, streaming, unsaved, no-agent, and pending-permission states.
- [ ] Empty state uses approved notebook/app-specific copy.
- [ ] Tool calls/results render as compact timeline rows, not peer chat bubbles.
- [ ] Pending permission renders as a distinct amber blocking action block.
- [ ] Composer includes scope status microcopy and preserves current disabled/submit behavior.
- [ ] Existing Tauri command names and payload shapes remain unchanged.
- [ ] Focused `ChatPanel` tests pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `ChatPanel.tsx` markup, class names, local helper functions, local display copy.
- OUT of scope: `stores/chat.ts`, Rust sidebar chat manager, Tauri command payloads, `NotebookSidebar.tsx`, multi-tab session UI, custom select/dropdown components.
- If a store/backend change appears necessary, emit `scope_drift` before editing outside `ChatPanel.tsx`.

**Implementation:**
- [ ] Add local helpers in `ChatPanel.tsx`:

```ts
function scopePathHint(path: string | undefined, appRoot: string | undefined) {
  return appRoot ?? path ?? "Save the notebook to chat";
}

function emptyStateCopy(isAppScope: boolean) {
  return isAppScope
    ? {
        title: "Ask inside this app context",
        body: "The assistant can inspect notebook cells, call app tools, and update panels.",
      }
    : {
        title: "Ask inside this notebook",
        body: "The assistant can inspect cells, draft edits, and explain outputs.",
      };
}

function statusCopy(args: {
  notebookPath: string | undefined;
  selectedAgentName: string;
  streaming: boolean;
  pendingPermission: boolean;
}) {
  if (!args.notebookPath) return "Save notebook to chat";
  if (!args.selectedAgentName) return "No chat agent configured";
  if (args.pendingPermission) return "Waiting for permission";
  if (args.streaming) return "Streaming in this session";
  return "Ready with scoped tools enabled";
}
```

- [ ] Replace `messageClassName` with distinct render paths:

```tsx
function renderMessage(message: ChatMessage) {
  if (message.kind === "toolCall") {
    return (
      <article className="grid grid-cols-[18px_1fr] gap-2 rounded border border-[#bfc1b7] bg-[#eeefe9] px-2 py-2 text-xs text-[#23251d]" key={message.id}>
        <div className="flex h-[18px] w-[18px] items-center justify-center rounded bg-[#fffefa] text-[10px] font-bold text-[#315f9f]">T</div>
        <div className="min-w-0">
          <div className="font-semibold">Tool call: {message.name}</div>
          <div className="truncate font-mono text-[10px] text-[#65675e]">{message.argsSummary}</div>
        </div>
      </article>
    );
  }

  if (message.kind === "toolResult") {
    return (
      <article className="grid grid-cols-[18px_1fr] gap-2 rounded border border-[#bfc1b7] bg-[#eeefe9] px-2 py-2 text-xs text-[#23251d]" key={message.id}>
        <div className="flex h-[18px] w-[18px] items-center justify-center rounded bg-[#fffefa] text-[10px] font-bold text-[#315f9f]">R</div>
        <div className="min-w-0">
          <div className="font-semibold">Tool result</div>
          <div className="truncate font-mono text-[10px] text-[#65675e]">{message.text}</div>
        </div>
      </article>
    );
  }

  const className =
    message.kind === "error"
      ? "rounded border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700"
      : "rounded border border-[#bfc1b7] bg-[#fffefa] px-3 py-2 text-sm text-[#23251d]";

  return (
    <article className={className} key={message.id}>
      <div className="whitespace-pre-wrap break-words">{message.text}</div>
    </article>
  );
}
```

- [ ] Restructure the header so scope is primary, agent/session controls are compact, and the status strip is visible:

```tsx
const isAppScope = Boolean(appOpenInfo?.app_root);
const scopeHint = scopePathHint(notebookPath, appOpenInfo?.app_root);
const emptyCopy = emptyStateCopy(isAppScope);
const currentStatus = statusCopy({
  notebookPath,
  selectedAgentName,
  streaming,
  pendingPermission: Boolean(pendingPermission),
});
```

- [ ] Preserve existing `select` elements with their current accessible names:
  - `aria-label="Agent"`
  - `aria-label="Agent session"`

- [ ] Preserve existing send button accessible name:

```tsx
aria-label="Send message"
```

- [ ] Run focused tests until green:

```bash
scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx
```

- [ ] Run typecheck:

```bash
scripts/spur-pnpm run typecheck
```

- [ ] Commit only the implementation file:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx
git commit -m "fix(spur-notebook): polish ai sidebar chat panel"
```

## DAG

```text
task-chat-panel-ux-tests
  -> task-chat-panel-ux-implementation
```

## Self-Review

- Spec coverage: header, transcript, permission, empty state, composer, and non-goals are represented.
- DAG validation: one test-first root task, one implementation task depending on tests.
- Placeholder scan: no TBD/TODO placeholders.
- Scope control: both tasks are limited to `ChatPanel` test/implementation files and explicitly exclude store/backend/multi-tab work.

