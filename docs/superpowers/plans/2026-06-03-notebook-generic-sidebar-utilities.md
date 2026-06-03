# Notebook Generic Sidebar Shell — Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-notebook-generic-sidebar-utilities-design.ipynb` (committed `305675aa`)
**Design epic:** n/a — lightweight design flow; spec committed directly (no design epic created)

**Goal:** Refactor the monolithic `DatasourceSidebar` into a generic, collapsible activity-bar **shell** (`NotebookSidebar`) that hosts pluggable utility **panels**, with datasources as the first panel.

**Architecture:** A new ephemeral zustand store (`useSidebar`) holds `{ activePanelId, collapsed }`. A static registry (`SIDEBAR_PANELS`) describes each panel `{ id, title, icon, ariaLabel, Component }`. `NotebookSidebar` renders an always-visible icon rail (one button per registry entry + a collapse toggle) and a panel region that **keep-alive mounts** panels (mount on first activation, stay mounted, hide inactive via the `hidden` attribute) so each panel's listeners stay live across switches. `DatasourceSidebar` becomes `DatasourcePanel` — its content with the shell chrome removed.

**Tech Stack:** React + TypeScript, zustand, Tailwind, lucide-react, Vitest + @testing-library/react.

**Working directory for all tasks:** `crates/spur-notebook/jute-notebook`
**Test commands:** single file → `npx vitest run <path>`; full suite → `npm run test`; types → `npm run typecheck`; lint → `npm run lint`.

**Deliberate deviation from spec §9:** `AddRestApiWizard.tsx` and `AddRestApiWizard.test.tsx` stay at `src/ui/notebook/` (imported via the `@/` alias). Moving them into `sidebar/` early would break the still-present `DatasourceSidebar.tsx` mid-sequence; keeping them in place lets every task compile green. Relocating them is a trivial optional follow-up, out of scope here.

---

## Dependency DAG

```
task-store ─┐
            ├─> task-shell ─> task-integrate
task-extract┘
```

- `task-store` and `task-extract` are independent roots (dispatch in parallel).
- `task-shell` depends on both (imports the store; registers `DatasourcePanel`).
- `task-integrate` depends on `task-shell` (renders `NotebookSidebar`, deletes the old file).

Every task leaves the workspace compiling and all tests green.

---

### Task 1: Sidebar store

**Task ID:** `task-store`

**Files:**
- Create: `src/stores/sidebar.ts`
- Test: `src/stores/__tests__/sidebar.test.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `useSidebar` exposes `{ activePanelId, collapsed, activatePanel, toggleCollapsed, setCollapsed }`.
- [ ] `activatePanel(id)` sets `activePanelId` and forces `collapsed = false`.
- [ ] `npx vitest run src/stores/__tests__/sidebar.test.ts` passes.
- [ ] `npm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `src/stores/sidebar.ts`, `src/stores/__tests__/sidebar.test.ts`.
- OUT of scope: any UI component, `panels.ts` (does not exist yet — do NOT import it).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** — `src/stores/__tests__/sidebar.test.ts`

```ts
import { beforeEach, describe, expect, test } from "vitest";

import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "../sidebar";

describe("useSidebar", () => {
  beforeEach(() => {
    useSidebar.setState({
      activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
      collapsed: false,
    });
  });

  test("starts on the default panel, expanded", () => {
    const state = useSidebar.getState();
    expect(state.activePanelId).toBe(DEFAULT_SIDEBAR_PANEL_ID);
    expect(state.collapsed).toBe(false);
  });

  test("activatePanel sets the id and clears collapsed", () => {
    useSidebar.setState({ collapsed: true });
    useSidebar.getState().activatePanel("chat");
    const state = useSidebar.getState();
    expect(state.activePanelId).toBe("chat");
    expect(state.collapsed).toBe(false);
  });

  test("toggleCollapsed flips collapsed", () => {
    useSidebar.getState().toggleCollapsed();
    expect(useSidebar.getState().collapsed).toBe(true);
    useSidebar.getState().toggleCollapsed();
    expect(useSidebar.getState().collapsed).toBe(false);
  });

  test("setCollapsed sets collapsed explicitly", () => {
    useSidebar.getState().setCollapsed(true);
    expect(useSidebar.getState().collapsed).toBe(true);
    useSidebar.getState().setCollapsed(false);
    expect(useSidebar.getState().collapsed).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npx vitest run src/stores/__tests__/sidebar.test.ts`
Expected: FAIL — cannot resolve `../sidebar`.

- [ ] **Step 3: Write the store** — `src/stores/sidebar.ts`

```ts
import { create } from "zustand";

export type SidebarState = {
  activePanelId: string;
  collapsed: boolean;
};

export type SidebarActions = {
  activatePanel: (id: string) => void;
  toggleCollapsed: () => void;
  setCollapsed: (collapsed: boolean) => void;
};

export type SidebarStore = SidebarState & SidebarActions;

// Must match SIDEBAR_PANELS[0].id in src/ui/notebook/sidebar/panels.ts.
export const DEFAULT_SIDEBAR_PANEL_ID = "datasources";

export const useSidebar = create<SidebarStore>()((set) => ({
  activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
  collapsed: false,

  activatePanel: (id) => set({ activePanelId: id, collapsed: false }),
  toggleCollapsed: () => set((state) => ({ collapsed: !state.collapsed })),
  setCollapsed: (collapsed) => set({ collapsed }),
}));
```

- [ ] **Step 4: Run to verify it passes**

Run: `npx vitest run src/stores/__tests__/sidebar.test.ts`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/stores/sidebar.ts src/stores/__tests__/sidebar.test.ts
git commit -m "feat(notebook): add ephemeral useSidebar store"
```

---

### Task 2: Extract `DatasourcePanel` from `DatasourceSidebar`

**Task ID:** `task-extract`

**Files:**
- Create: `src/ui/notebook/sidebar/DatasourcePanel.tsx` (content extracted from `src/ui/notebook/DatasourceSidebar.tsx`)
- Create: `src/ui/notebook/sidebar/DatasourcePanel.test.tsx` (adapted from `src/ui/notebook/DatasourceSidebar.test.tsx`)
- Leave untouched: `src/ui/notebook/DatasourceSidebar.tsx`, `src/ui/notebook/DatasourceSidebar.test.tsx`, `src/ui/notebook/AddRestApiWizard.tsx` (deleted/handled later)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `DatasourcePanel` is the body of the old sidebar with **all shell chrome removed**: no `collapsed` state, no `<aside>` wrappers, no collapsed-rail branch, no header row (DatabaseIcon + "Datasources" title + collapse chevron).
- [ ] All datasource behavior preserved: daemon logic, the four listeners, group input, Add/API buttons, dropzone, error display, grouped list, saved connections, and the `AddRestApiWizard` modal.
- [ ] `restWizardPrefillFromPayload` is still exported from `DatasourcePanel.tsx`.
- [ ] No `activatePanel` call yet (added in `task-shell`).
- [ ] `npx vitest run src/ui/notebook/sidebar/DatasourcePanel.test.tsx` passes.
- [ ] `npm run typecheck` clean (old `DatasourceSidebar.tsx` still compiles and is still used by `NotebookView`).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files under `src/ui/notebook/sidebar/`.
- OUT of scope: `DatasourceSidebar.tsx`, `NotebookView.tsx`, `AddRestApiWizard.tsx`, the store. Do NOT delete the old files and do NOT change `NotebookView`.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Create `DatasourcePanel.tsx` by transforming the old file**

Start from the current `src/ui/notebook/DatasourceSidebar.tsx` and apply exactly these changes:

1. Rename the default export function `DatasourceSidebar` → `DatasourcePanel`.
2. Fix the wizard import path so it resolves from the new folder. Change:
   ```ts
   import AddRestApiWizard, { type AddRestApiWizardPrefill } from "./AddRestApiWizard";
   ```
   to:
   ```ts
   import AddRestApiWizard, {
     type AddRestApiWizardPrefill,
   } from "@/ui/notebook/AddRestApiWizard";
   ```
   (All other `@/...` imports are unchanged. `restWizardPrefillFromPayload` stays exported with its existing `// eslint-disable-next-line react-refresh/only-export-components` comment.)
3. Delete the collapse state declaration:
   ```ts
   const [collapsed, setCollapsed] = useState(false);
   ```
4. Delete the entire collapsed-branch early return block:
   ```tsx
   if (collapsed) {
     return (
       <aside ...>
         ...
         <DatabaseIcon className="mt-4" size={18} strokeWidth={1.5} />
       </aside>
     );
   }
   ```
5. Replace the outer return wrapper. The current return is:
   ```tsx
   return (
     <>
       <aside className="flex h-full w-80 shrink-0 flex-col border-l border-gray-200 bg-gray-50 text-gray-700">
         <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-14">
           <div className="flex items-center justify-between gap-2"> ...header row... </div>
           ...rest of content...
         </div>
       </aside>
       <AddRestApiWizard ... />
     </>
   );
   ```
   Transform it to drop the `<aside>` and the header row, keeping the rest of the content in a full-height scroll column:
   ```tsx
   return (
     <>
       <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-3 text-gray-700">
         {/* header row REMOVED — the shell renders the panel title */}
         ...all the remaining content unchanged (group/add controls, dropzone,
            error block, "In this notebook" list, SavedConnectionsSection)...
       </div>
       <AddRestApiWizard ... />
     </>
   );
   ```
   (Note the top padding drops from `pt-14` to `pt-3`: the shell now owns the `pt-14` offset on the panel header. Keep `pb-16`.)
6. Remove now-unused imports: `ChevronLeftIcon` (only used by the deleted collapsed branch). Keep `ChevronRightIcon` / `ChevronDownIcon` (still used by `SavedConnectionRow`) and `DatabaseIcon` only if still referenced — after removing the header and collapsed branch, `DatabaseIcon` is unused, so remove it from the `lucide-react` import. Remove `useState` from the React import only if no other `useState` remains (it does remain — many `useState` calls persist — so KEEP `useState`).
7. Keep ALL helper functions and sub-components (`SavedConnectionsSection`, `SavedConnectionRow`, `DatasourceListItem`, `DatasourceKindBadge`, `DatasourceColumnRow`, `groupDatasourceEntries`, `upsertDatasourceEntry`, `datasourceNameFromPath`, `normalizeGroup`, `firstSelectedPath`, `firstDroppedPath`, `isDatasourcePath`, `isPositionInsideElement`, `errorMessage`) exactly as they are.

- [ ] **Step 2: Create `DatasourcePanel.test.tsx` from the old test**

Copy `src/ui/notebook/DatasourceSidebar.test.tsx` to `src/ui/notebook/sidebar/DatasourcePanel.test.tsx` and apply:
1. Change the import:
   ```ts
   import DatasourceSidebar, {
     restWizardPrefillFromPayload,
   } from "./DatasourceSidebar";
   ```
   to:
   ```ts
   import DatasourcePanel, {
     restWizardPrefillFromPayload,
   } from "./DatasourcePanel";
   ```
2. Replace every `render(<DatasourceSidebar />)` with `render(<DatasourcePanel />)` and the `describe("DatasourceSidebar", ...)` label with `describe("DatasourcePanel", ...)`.
3. All `vi.mock("@/daemon/control", ...)`, `@tauri-apps/*` mocks, and helpers are unchanged (the `@/` and `@tauri-apps/*` specifiers resolve identically from the new folder).

- [ ] **Step 3: Run the new test**

Run: `npx vitest run src/ui/notebook/sidebar/DatasourcePanel.test.tsx`
Expected: PASS (same count as the old suite).

- [ ] **Step 4: Verify the whole project still compiles**

Run: `npm run typecheck`
Expected: clean — old `DatasourceSidebar.tsx` is untouched and still imported by `NotebookView`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/notebook/sidebar/DatasourcePanel.tsx src/ui/notebook/sidebar/DatasourcePanel.test.tsx
git commit -m "refactor(notebook): extract DatasourcePanel from DatasourceSidebar"
```

**Scope Drift Checkpoint:**
- If the transformation requires editing `DatasourceSidebar.tsx` or `NotebookView.tsx` to stay green → emit `scope_drift` (it should not; the old file is self-contained).

---

### Task 3: Shell, registry, types, and activation wiring

**Task ID:** `task-shell`

**Files:**
- Create: `src/ui/notebook/sidebar/types.ts`
- Create: `src/ui/notebook/sidebar/panels.ts`
- Create: `src/ui/notebook/sidebar/NotebookSidebar.tsx`
- Create: `src/ui/notebook/sidebar/NotebookSidebar.test.tsx`
- Modify: `src/ui/notebook/sidebar/DatasourcePanel.tsx` (add the `activatePanel` call in the `open_rest_wizard` listener)
- Modify: `src/ui/notebook/sidebar/DatasourcePanel.test.tsx` (assert activation in the `open_rest_wizard` test)

**Depends on:** `task-store`, `task-extract`

**Acceptance Criteria:**
- [ ] `SidebarPanel` type and `SIDEBAR_PANELS` registry exist; the first entry has `id: "datasources"` (matching `DEFAULT_SIDEBAR_PANEL_ID`).
- [ ] `NotebookSidebar` renders an icon-rail button per registry entry plus a collapse toggle; the active panel's title shows in a header; inactive panels are kept mounted but `hidden`; lazy-mount (a panel is not in the DOM until first activated).
- [ ] The `open_rest_wizard` listener in `DatasourcePanel` calls `useSidebar.getState().activatePanel("datasources")` before opening the wizard.
- [ ] `npx vitest run src/ui/notebook/sidebar/NotebookSidebar.test.tsx src/ui/notebook/sidebar/DatasourcePanel.test.tsx` passes.
- [ ] `npm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the four new files under `src/ui/notebook/sidebar/`, plus the two listed edits to `DatasourcePanel.tsx` / `DatasourcePanel.test.tsx`.
- OUT of scope: `NotebookView.tsx` (still renders the OLD sidebar — do NOT change it here), the old `DatasourceSidebar.tsx`, the store file.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Create `types.ts`**

```ts
import type { LucideIcon } from "lucide-react";
import type { ComponentType } from "react";

export type SidebarPanel = {
  id: string;
  title: string;
  icon: LucideIcon;
  ariaLabel: string;
  Component: ComponentType;
};
```

- [ ] **Step 2: Create `panels.ts`**

```ts
import { DatabaseIcon } from "lucide-react";

import DatasourcePanel from "./DatasourcePanel";
import type { SidebarPanel } from "./types";

export const SIDEBAR_PANELS: SidebarPanel[] = [
  {
    id: "datasources",
    title: "Datasources",
    icon: DatabaseIcon,
    ariaLabel: "Datasources",
    Component: DatasourcePanel,
  },
];
```

- [ ] **Step 3: Write the failing shell test** — `src/ui/notebook/sidebar/NotebookSidebar.test.tsx`

This test mocks the registry so it exercises only shell logic (no daemon/Tauri).

```tsx
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "@/stores/sidebar";

vi.mock("./panels", () => {
  const Stub = (label: string) => () => <div>{label}</div>;
  const Icon = () => <span data-testid="icon" />;
  return {
    SIDEBAR_PANELS: [
      { id: "datasources", title: "Datasources", ariaLabel: "Datasources", icon: Icon, Component: Stub("ALPHA BODY") },
      { id: "chat", title: "AI chat", ariaLabel: "AI chat", icon: Icon, Component: Stub("BETA BODY") },
    ],
  };
});

import NotebookSidebar from "./NotebookSidebar";

beforeEach(() => {
  useSidebar.setState({ activePanelId: DEFAULT_SIDEBAR_PANEL_ID, collapsed: false });
});
afterEach(cleanup);

describe("NotebookSidebar", () => {
  test("renders one rail button per panel plus a collapse toggle", () => {
    render(<NotebookSidebar />);
    expect(screen.getByRole("button", { name: "Datasources" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "AI chat" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /collapse sidebar/i })).toBeInTheDocument();
  });

  test("lazy-mounts: inactive panel is absent until activated, then stays mounted", () => {
    render(<NotebookSidebar />);
    expect(screen.getByText("ALPHA BODY")).toBeVisible();
    expect(screen.queryByText("BETA BODY")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "AI chat" }));
    expect(screen.getByText("BETA BODY")).toBeVisible();
    // alpha kept mounted but hidden
    expect(screen.getByText("ALPHA BODY")).not.toBeVisible();
  });

  test("collapse toggle hides panels and flips its label", () => {
    render(<NotebookSidebar />);
    fireEvent.click(screen.getByRole("button", { name: /collapse sidebar/i }));
    expect(screen.getByText("ALPHA BODY")).not.toBeVisible();
    expect(screen.getByRole("button", { name: /expand sidebar/i })).toBeInTheDocument();
  });

  test("activating a panel while collapsed expands", () => {
    useSidebar.setState({ collapsed: true });
    render(<NotebookSidebar />);
    fireEvent.click(screen.getByRole("button", { name: "AI chat" }));
    expect(screen.getByText("BETA BODY")).toBeVisible();
    expect(useSidebar.getState().collapsed).toBe(false);
  });
});
```

- [ ] **Step 4: Run to verify it fails**

Run: `npx vitest run src/ui/notebook/sidebar/NotebookSidebar.test.tsx`
Expected: FAIL — cannot resolve `./NotebookSidebar`.

- [ ] **Step 5: Create `NotebookSidebar.tsx`**

```tsx
import clsx from "clsx";
import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react";
import { useEffect, useState } from "react";

import { useSidebar } from "@/stores/sidebar";

import { SIDEBAR_PANELS } from "./panels";

export default function NotebookSidebar() {
  const activePanelId = useSidebar((state) => state.activePanelId);
  const collapsed = useSidebar((state) => state.collapsed);
  const activatePanel = useSidebar((state) => state.activatePanel);
  const toggleCollapsed = useSidebar((state) => state.toggleCollapsed);

  const activePanel =
    SIDEBAR_PANELS.find((panel) => panel.id === activePanelId) ??
    SIDEBAR_PANELS[0];

  // Keep-alive: a panel mounts on first activation and stays mounted.
  const [mountedIds, setMountedIds] = useState<string[]>([activePanel.id]);
  useEffect(() => {
    setMountedIds((ids) =>
      ids.includes(activePanel.id) ? ids : [...ids, activePanel.id],
    );
  }, [activePanel.id]);

  const ActiveIcon = activePanel.icon;

  return (
    <aside className="flex h-full shrink-0 border-l border-gray-200 bg-gray-50 text-gray-700">
      <div
        className={clsx(
          "flex h-full min-h-0 flex-col overflow-hidden transition-[width] duration-200",
          collapsed ? "w-0" : "w-80",
        )}
      >
        {!collapsed && (
          <div className="flex items-center gap-2 px-3 pb-2 pt-14">
            <ActiveIcon className="shrink-0 text-gray-500" size={18} />
            <h2 className="truncate text-sm font-medium text-gray-950">
              {activePanel.title}
            </h2>
          </div>
        )}
        {SIDEBAR_PANELS.map((panel) => {
          if (!mountedIds.includes(panel.id)) return null;
          const PanelComponent = panel.Component;
          return (
            <div
              className="min-h-0 flex-1"
              hidden={collapsed || panel.id !== activePanel.id}
              key={panel.id}
            >
              <PanelComponent />
            </div>
          );
        })}
      </div>

      <div className="flex w-12 shrink-0 flex-col items-center gap-1 border-l border-gray-200 bg-white pt-14">
        <button
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          className="rounded p-1.5 text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-950"
          onClick={toggleCollapsed}
          type="button"
        >
          {collapsed ? (
            <ChevronLeftIcon size={18} strokeWidth={1.5} />
          ) : (
            <ChevronRightIcon size={18} strokeWidth={1.5} />
          )}
        </button>
        <div className="my-1 h-px w-5 bg-gray-200" />
        {SIDEBAR_PANELS.map((panel) => {
          const Icon = panel.icon;
          const isActive = !collapsed && panel.id === activePanel.id;
          return (
            <button
              aria-label={panel.ariaLabel}
              aria-pressed={isActive}
              className={clsx(
                "rounded p-1.5 transition-colors",
                isActive
                  ? "bg-gray-900 text-white"
                  : "text-gray-500 hover:bg-gray-100 hover:text-gray-950",
              )}
              key={panel.id}
              onClick={() => activatePanel(panel.id)}
              type="button"
            >
              <Icon size={18} strokeWidth={1.5} />
            </button>
          );
        })}
      </div>
    </aside>
  );
}
```

- [ ] **Step 6: Run the shell test**

Run: `npx vitest run src/ui/notebook/sidebar/NotebookSidebar.test.tsx`
Expected: PASS (4 tests).

- [ ] **Step 7: Wire `activatePanel` into `DatasourcePanel.tsx`**

Add the store import at the top of `DatasourcePanel.tsx`:
```ts
import { useSidebar } from "@/stores/sidebar";
```
In the `useEffect` that listens for `notebook://open_rest_wizard`, inside the handler, before `setApiModalOpen(true)`, add the activation call. The handler becomes:
```ts
void listen("notebook://open_rest_wizard", (event) => {
  const nextPrefill = restWizardPrefillFromPayload(event.payload);
  if (!nextPrefill) return;

  useSidebar.getState().activatePanel("datasources");
  setEditingConnection(null);
  setRestWizardPrefill(nextPrefill);
  setApiModalOpen(true);
})
```
(Use `useSidebar.getState()` — an imperative call inside a non-React event handler — rather than a hook selector.)

- [ ] **Step 8: Extend the `open_rest_wizard` test in `DatasourcePanel.test.tsx`**

At the top of the file add:
```ts
import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "@/stores/sidebar";
```
In the existing `beforeEach` (or add one) reset the store:
```ts
useSidebar.setState({ activePanelId: DEFAULT_SIDEBAR_PANEL_ID, collapsed: false });
```
In the test that dispatches the `notebook://open_rest_wizard` event, after asserting the wizard opened, add:
```ts
expect(useSidebar.getState().activePanelId).toBe("datasources");
```
(If that test first sets a different active panel to prove the switch, call `useSidebar.getState().activatePanel("chat")` before dispatching the event, then assert it flipped back to `"datasources"`.)

- [ ] **Step 9: Run both affected tests + typecheck**

Run: `npx vitest run src/ui/notebook/sidebar/NotebookSidebar.test.tsx src/ui/notebook/sidebar/DatasourcePanel.test.tsx`
Expected: PASS.
Run: `npm run typecheck`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add src/ui/notebook/sidebar/types.ts src/ui/notebook/sidebar/panels.ts src/ui/notebook/sidebar/NotebookSidebar.tsx src/ui/notebook/sidebar/NotebookSidebar.test.tsx src/ui/notebook/sidebar/DatasourcePanel.tsx src/ui/notebook/sidebar/DatasourcePanel.test.tsx
git commit -m "feat(notebook): add NotebookSidebar shell, registry, and panel activation"
```

---

### Task 4: Integrate shell into `NotebookView` and remove the old sidebar

**Task ID:** `task-integrate`

**Files:**
- Modify: `src/ui/notebook/NotebookView.tsx` (lines 8, 47)
- Delete: `src/ui/notebook/DatasourceSidebar.tsx`
- Delete: `src/ui/notebook/DatasourceSidebar.test.tsx`

**Depends on:** `task-shell`

**Acceptance Criteria:**
- [ ] `NotebookView` imports and renders `<NotebookSidebar />` instead of `<DatasourceSidebar />`.
- [ ] `DatasourceSidebar.tsx` and `DatasourceSidebar.test.tsx` are deleted; no remaining references to `DatasourceSidebar` anywhere in `src/`.
- [ ] `npm run test` (full suite) passes.
- [ ] `npm run typecheck` and `npm run lint` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `NotebookView.tsx` and deleting the two old files.
- OUT of scope: anything under `src/ui/notebook/sidebar/` (already complete), the store, `AddRestApiWizard.tsx`.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Swap the import in `NotebookView.tsx`**

Change line 8:
```ts
import DatasourceSidebar from "./DatasourceSidebar";
```
to:
```ts
import NotebookSidebar from "./sidebar/NotebookSidebar";
```

- [ ] **Step 2: Swap the element in `NotebookView.tsx`**

Change line 47:
```tsx
      <DatasourceSidebar />
```
to:
```tsx
      <NotebookSidebar />
```

- [ ] **Step 3: Delete the old files**

```bash
git rm src/ui/notebook/DatasourceSidebar.tsx src/ui/notebook/DatasourceSidebar.test.tsx
```

- [ ] **Step 4: Verify no dangling references**

Run: `grep -rn "DatasourceSidebar" src` (expect no results).

- [ ] **Step 5: Run the full suite + typecheck + lint**

Run: `npm run test`
Expected: PASS (the new `DatasourcePanel` and `NotebookSidebar` suites run; the old duplicate is gone).
Run: `npm run typecheck && npm run lint`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/ui/notebook/NotebookView.tsx
git commit -m "feat(notebook): mount NotebookSidebar shell; remove DatasourceSidebar"
```

---

## Self-Review

**1. Spec coverage:**
- §4 architecture (shell + rail + panel region) → `task-shell` (`NotebookSidebar.tsx`).
- §5 collapse/activation behavior → `task-shell` (rail toggle + `activatePanel`).
- §6 `useSidebar` store → `task-store`.
- §7 keep-alive mounting + listener liveness + programmatic activation → `task-shell` (lazy+sticky `mountedIds`, `hidden`; `activatePanel` call in `DatasourcePanel`).
- §8 panel interface & registry → `task-shell` (`types.ts`, `panels.ts`).
- §9 `DatasourceSidebar` → `DatasourcePanel` refactor → `task-extract` (+ `NotebookView` swap and old-file delete in `task-integrate`). Deviation: `AddRestApiWizard` stays in place (documented above).
- §10 dropzone caveat → preserved implicitly (dropzone stays in `DatasourcePanel`; no behavior change required).
- §11 testing → store test (`task-store`), migrated panel test (`task-extract`/`task-shell`), shell test (`task-shell`).
- §12 build sequence → tasks ordered store ∥ extract → shell → integrate; each leaves the build green.

**2. Placeholder scan:** No TBD/TODO; every code step has concrete content; transformation steps reference exact symbols/lines.

**3. Type consistency:** `DEFAULT_SIDEBAR_PANEL_ID` ("datasources") matches `SIDEBAR_PANELS[0].id`; `SidebarPanel` fields used consistently by `panels.ts` and `NotebookSidebar.tsx`; store selector/`getState()` usage consistent.

**4. DAG validation:** `task-store` ∥ `task-extract` → `task-shell` → `task-integrate`. Acyclic; two parallel roots; single integration tail.

**5. beads compatibility:** Each task has a unique ID, an explicit `depends_on`, verifiable acceptance criteria (named test commands), and a scope boundary with `scope_drift` guidance.
