# Notebook Tabs Browser-Grade UI/UX Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-notebook-tabs-browser-grade-uiux.md`
(builds on `docs/superpowers/specs/2026-06-09-notebook-multi-tab-design.md`)
**Design epic:** open-design session 2026-06-12, approved board `~/.spur/scratch/Untitled115.ipynb`

**Goal:** Bring the shipped Jute tab strip to browser-grade behavior: dynamic widths +
width-lock, pinning, searchable tab list, hover kernel card, context menu, reopen-closed,
attention state, drag reorder, and upgraded keyboard handling.

**Architecture:** Frontend-only. Extend the `useNotebookTabsStore` zustand model (pinned,
attention, closed stack, move/pin actions), rework `NotebookTabStrip` anatomy, add three new
presentational components (`TabHoverCard`, `TabContextMenu`, `TabListMenu`), wire them in an
integration pass, and persist pin/order state in the existing URL route model.

**Tech Stack:** React 18, zustand, Tailwind, wouter, Vitest + Testing Library. All test runs
through `scripts/spur-pnpm` (never bare pnpm).

**Worker note:** every task is routed to `codex`. Commit after each task with the message
given in the task. Run only the focused test commands listed; the final task runs the sweep.

---

### Task 1: Tabs store model + actions

**Task ID:** `task-tabs-store`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` (tabs-store section, lines ~265-314)
- Create: `crates/spur-notebook/jute-notebook/src/stores/__tests__/notebook-tabs.test.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `NotebookTab` gains optional `pinned`, `attention`, `kernelGeneration` fields
- [ ] `setPinned` re-anchors the tab to the boundary of the pinned group
- [ ] `moveTab` clamps so unpinned tabs cannot enter the pinned region and vice versa
- [ ] `updateTab` sets `attention: true` when a non-active tab leaves `running`
- [ ] `setActiveTabId` clears `attention` on the activated tab
- [ ] Closed-tab stack is LIFO, capped at 10
- [ ] `scripts/spur-pnpm test -- src/stores/__tests__/notebook-tabs.test.ts` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the `NotebookTab` type, `NotebookTabsStore` type, `useNotebookTabsStore` create
  block, the new test file.
- OUT of scope: `Notebook` class, viewState slices, any component file, `NotebookPage.tsx`.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```ts
// crates/spur-notebook/jute-notebook/src/stores/__tests__/notebook-tabs.test.ts
import { beforeEach, describe, expect, it } from "vitest";

import { useNotebookTabsStore, type NotebookTab } from "@/stores/notebook";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: id,
  dirty: false,
  kernelState: "idle",
  mode: "cells",
  ...patch,
});

beforeEach(() => {
  useNotebookTabsStore.setState({
    tabs: [],
    activeTabId: undefined,
    closedTabs: [],
  });
});

describe("pinning", () => {
  it("pins a tab to the end of the pinned group", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("a"), tab("b")],
    });
    useNotebookTabsStore.getState().setPinned("b", true);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p1",
      "b",
      "a",
    ]);
    expect(useNotebookTabsStore.getState().tabs[1].pinned).toBe(true);
  });

  it("unpins a tab to the start of the unpinned group", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("p2", { pinned: true }), tab("a")],
    });
    useNotebookTabsStore.getState().setPinned("p1", false);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p2",
      "p1",
      "a",
    ]);
  });
});

describe("moveTab", () => {
  it("reorders unpinned tabs", () => {
    useNotebookTabsStore.setState({ tabs: [tab("a"), tab("b"), tab("c")] });
    useNotebookTabsStore.getState().moveTab("c", 0);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "c",
      "a",
      "b",
    ]);
  });

  it("clamps unpinned tabs out of the pinned region", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("a"), tab("b")],
    });
    useNotebookTabsStore.getState().moveTab("b", 0);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p1",
      "b",
      "a",
    ]);
  });
});

describe("attention", () => {
  it("marks a background tab when its run finishes", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { kernelState: "running" }), tab("b")],
      activeTabId: "b",
    });
    useNotebookTabsStore.getState().updateTab("a", { kernelState: "live" });
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBe(true);
  });

  it("does not mark the active tab", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { kernelState: "running" })],
      activeTabId: "a",
    });
    useNotebookTabsStore.getState().updateTab("a", { kernelState: "live" });
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBeFalsy();
  });

  it("clears attention when the tab becomes active", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { attention: true }), tab("b")],
      activeTabId: "b",
    });
    useNotebookTabsStore.getState().setActiveTabId("a");
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBe(false);
  });
});

describe("closed-tab stack", () => {
  it("pops in LIFO order and caps at 10", () => {
    const store = useNotebookTabsStore.getState();
    for (let i = 0; i < 12; i += 1) {
      store.pushClosedTab({ tab: tab(`t${i}`), index: i });
    }
    expect(useNotebookTabsStore.getState().closedTabs).toHaveLength(10);
    expect(useNotebookTabsStore.getState().popClosedTab()?.tab.id).toBe("t11");
    expect(useNotebookTabsStore.getState().closedTabs).toHaveLength(9);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/stores/__tests__/notebook-tabs.test.ts`
Expected: FAIL (`setPinned` / `moveTab` / `closedTabs` do not exist)

- [ ] **Step 3: Implement the store changes**

In `src/stores/notebook.ts`, extend the existing types and store (keep existing fields and
actions intact):

```ts
export type NotebookTab = {
  id: string;
  path?: string;
  title: string;
  dirty: boolean;
  kernelState: NotebookTabKernelState;
  language?: string;
  mode: NotebookViewMode;
  pinned?: boolean;
  attention?: boolean;
  kernelGeneration?: number;
};

export type ClosedTabRecord = { tab: NotebookTab; index: number };

type NotebookTabsStore = {
  tabs: NotebookTab[];
  activeTabId?: string;
  closedTabs: ClosedTabRecord[];
  setTabs: (tabs: NotebookTab[]) => void;
  setActiveTabId: (tabId: string | undefined) => void;
  updateTab: (tabId: string, patch: Partial<NotebookTab>) => void;
  setPinned: (tabId: string, pinned: boolean) => void;
  moveTab: (tabId: string, toIndex: number) => void;
  pushClosedTab: (record: ClosedTabRecord) => void;
  popClosedTab: () => ClosedTabRecord | undefined;
};
```

```ts
export const useNotebookTabsStore = create<NotebookTabsStore>((set, get) => ({
  tabs: [],
  activeTabId: undefined,
  closedTabs: [],
  setTabs: (tabs) =>
    set((state) => {
      const activeTabStillOpen =
        state.activeTabId !== undefined &&
        tabs.some((tab) => tab.id === state.activeTabId);
      return {
        tabs,
        activeTabId: activeTabStillOpen
          ? state.activeTabId
          : (tabs[0]?.id ?? undefined),
      };
    }),
  setActiveTabId: (tabId) =>
    set((state) => {
      if (tabId !== undefined && !state.tabs.some((tab) => tab.id === tabId)) {
        return state;
      }
      return {
        activeTabId: tabId,
        tabs: state.tabs.map((tab) =>
          tab.id === tabId && tab.attention
            ? { ...tab, attention: false }
            : tab,
        ),
      };
    }),
  updateTab: (tabId, patch) =>
    set((state) => ({
      tabs: state.tabs.map((tab) => {
        if (tab.id !== tabId) return tab;
        const next = { ...tab, ...patch, id: tab.id };
        const finishedInBackground =
          tab.kernelState === "running" &&
          next.kernelState !== "running" &&
          state.activeTabId !== tabId;
        if (finishedInBackground) next.attention = true;
        return next;
      }),
    })),
  setPinned: (tabId, pinned) =>
    set((state) => {
      const tab = state.tabs.find((candidate) => candidate.id === tabId);
      if (!tab || Boolean(tab.pinned) === pinned) return state;
      const rest = state.tabs.filter((candidate) => candidate.id !== tabId);
      const pinnedCount = rest.filter((candidate) => candidate.pinned).length;
      const next = [...rest];
      next.splice(pinnedCount, 0, { ...tab, pinned });
      return { tabs: next };
    }),
  moveTab: (tabId, toIndex) =>
    set((state) => {
      const from = state.tabs.findIndex((candidate) => candidate.id === tabId);
      if (from < 0) return state;
      const tab = state.tabs[from];
      const next = state.tabs.filter((candidate) => candidate.id !== tabId);
      const pinnedCount = next.filter((candidate) => candidate.pinned).length;
      const clamped = tab.pinned
        ? Math.min(Math.max(toIndex, 0), pinnedCount)
        : Math.min(Math.max(toIndex, pinnedCount), next.length);
      next.splice(clamped, 0, tab);
      return { tabs: next };
    }),
  pushClosedTab: (record) =>
    set((state) => ({ closedTabs: [...state.closedTabs.slice(-9), record] })),
  popClosedTab: () => {
    const stack = get().closedTabs;
    const top = stack[stack.length - 1];
    if (top) set({ closedTabs: stack.slice(0, -1) });
    return top;
  },
}));
```

Note: the previous `updateTab` carried a no-op `activeTabId: get().activeTabId`; it is
intentionally dropped.

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-pnpm test -- src/stores/__tests__/notebook-tabs.test.ts`
Expected: PASS. Also run `scripts/spur-pnpm test -- src/stores/__tests__/notebook.test.ts`
to confirm no regression in the existing store suite.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/stores/notebook.ts crates/spur-notebook/jute-notebook/src/stores/__tests__/notebook-tabs.test.ts
git commit -m "feat(spur-notebook): tabs store pin move attention reopen model"
```

---

### Task 2: Tab strip anatomy: widths, width-lock, pinned, attention, gestures

**Task ID:** `task-strip-anatomy`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx`
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx`

**Depends on:** `task-tabs-store`

**Acceptance Criteria:**
- [ ] Tabs use `min-w-[56px] max-w-[200px] flex-1 basis-[200px]` sizing (replaces `min-w-[116px]`)
- [ ] Pinned tabs render icon-only at `w-[42px]`: kernel dot + badge, no title, no close button
- [ ] Attention tabs render `bg-green-50` with a green "✓" suffix after the title
- [ ] Middle-click (auxclick button 1) on a non-pinned tab calls `onCloseTab`
- [ ] Double-click on empty strip area calls `onNewTab`
- [ ] `+` button renders immediately after the last tab (inside the scrollable row), ▾ stays at the right edge
- [ ] After a close-click, the strip enters width-lock (`data-width-lock="true"`, per-tab fixed
      pixel widths) and releases on `mouseLeave` of the strip
- [ ] Tab row is horizontally scrollable when overflowing (`overflow-x-auto`)
- [ ] Progressive disclosure: when the estimated per-tab width drops below 96px unpinned tabs
      hide the close slot, below 66px they also hide the badge (kernel dot never yields)
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `NotebookTabStrip.tsx` and its new test file only.
- OUT of scope: `NotebookPage.tsx`, stores, new overlay components (later tasks).
- Keep the existing props contract (`activeTabId, tabs, onCloseTab, onNewTab, onOpenNotebook,
  onSwitchTab`) working; the old ▾ menu (New/Open) stays as-is in this task.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import NotebookTabStrip from "@/ui/notebook/NotebookTabStrip";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: `${id}.ipynb`,
  dirty: false,
  kernelState: "live",
  mode: "cells",
  ...patch,
});

function renderStrip(tabs: NotebookTab[], overrides = {}) {
  const props = {
    activeTabId: tabs[0]?.id,
    tabs,
    onCloseTab: vi.fn(),
    onNewTab: vi.fn(),
    onOpenNotebook: vi.fn(),
    onSwitchTab: vi.fn(),
    ...overrides,
  };
  const view = render(<NotebookTabStrip {...props} />);
  return { ...view, props };
}

it("renders pinned tabs icon-only without a close button", () => {
  renderStrip([tab("pin", { pinned: true }), tab("a")]);
  expect(screen.queryByLabelText("Close pin.ipynb")).toBeNull();
  expect(screen.queryByText("pin.ipynb")).toBeNull();
});

it("closes a non-pinned tab on middle click", () => {
  const { props } = renderStrip([tab("a"), tab("b")]);
  fireEvent.auxClick(screen.getByRole("tab", { name: /b\.ipynb/ }), {
    button: 1,
  });
  expect(props.onCloseTab).toHaveBeenCalledWith("b");
});

it("ignores middle click on pinned tabs", () => {
  const { props } = renderStrip([tab("pin", { pinned: true }), tab("a")]);
  fireEvent.auxClick(screen.getByLabelText("pin.ipynb (pinned)"), {
    button: 1,
  });
  expect(props.onCloseTab).not.toHaveBeenCalled();
});

it("creates a tab on double-clicking empty strip area", () => {
  const { props } = renderStrip([tab("a")]);
  fireEvent.doubleClick(screen.getByRole("tablist"));
  expect(props.onNewTab).toHaveBeenCalled();
});

it("marks an attention tab with a tick", () => {
  renderStrip([tab("a"), tab("b", { attention: true })]);
  const attn = screen.getByRole("tab", { name: /b\.ipynb/ }).closest("div");
  expect(attn?.className).toContain("bg-green-50");
});

it("enters width-lock after close and releases on mouse leave", () => {
  const { props } = renderStrip([tab("a"), tab("b"), tab("c")]);
  fireEvent.click(screen.getByLabelText("Close b.ipynb"));
  expect(props.onCloseTab).toHaveBeenCalledWith("b");
  const strip = screen.getByTestId("tab-strip");
  expect(strip.dataset.widthLock).toBe("true");
  fireEvent.mouseLeave(strip);
  expect(strip.dataset.widthLock).toBeUndefined();
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx`
Expected: FAIL (pinned rendering, width-lock, gestures not implemented)

- [ ] **Step 3: Implement**

Rework `NotebookTabStrip.tsx`. Key structure (keep Jute classes; full file is the worker's
to shape, these parts are mandatory):

```tsx
const [lockedWidths, setLockedWidths] = useState<Record<string, number> | null>(
  null,
);
const tabRefs = useRef(new Map<string, HTMLDivElement>());

const captureWidths = () => {
  const widths: Record<string, number> = {};
  tabRefs.current.forEach((el, id) => {
    widths[id] = el.getBoundingClientRect().width;
  });
  setLockedWidths(widths);
};

const handleClose = (tabId: string) => {
  captureWidths();
  onCloseTab(tabId);
};
```

Strip container: `data-testid="tab-strip"`, `data-width-lock` set when `lockedWidths !== null`,
`onMouseLeave={() => setLockedWidths(null)}`.

Tab row container (`role="tablist"`): `overflow-x-auto`, `onDoubleClick` calling `onNewTab`
when `event.target === event.currentTarget`. The `+` button moves INSIDE this row, after the
mapped tabs; the ▾ overflow button stays outside at the right edge.

Per-tab outer div:

```tsx
<div
  ref={(el) => {
    if (el) tabRefs.current.set(tab.id, el);
    else tabRefs.current.delete(tab.id);
  }}
  style={
    lockedWidths?.[tab.id] !== undefined
      ? { width: lockedWidths[tab.id], flex: "0 0 auto" }
      : undefined
  }
  onAuxClick={(event) => {
    if (event.button === 1 && !tab.pinned) handleClose(tab.id);
  }}
  className={clsx(
    "group relative flex h-8 items-center rounded-t border border-b-0 text-xs",
    tab.pinned
      ? "w-[42px] flex-none justify-center px-0"
      : "min-w-[56px] max-w-[200px] flex-1 basis-[200px] px-2",
    tab.attention && "bg-green-50",
    active
      ? "z-10 border-gray-200 bg-white"
      : !tab.attention &&
          "border-transparent bg-gray-50 text-gray-500 hover:bg-gray-100 hover:text-gray-900",
  )}
  aria-label={tab.pinned ? `${tab.title} (pinned)` : undefined}
>
```

Pinned tabs render only `dot + badge` (no title span, no close slot). Attention tabs append
`<span aria-hidden="true" className="ml-1 text-[10px] text-green-600">✓</span>` after the title.
Close button calls `handleClose` instead of `onCloseTab` directly.

Progressive disclosure (no per-tab measurement; estimate from the row width):

```tsx
const [rowWidth, setRowWidth] = useState<number | null>(null);
const rowRef = useRef<HTMLDivElement>(null);

useEffect(() => {
  if (typeof ResizeObserver === "undefined" || !rowRef.current) return;
  const observer = new ResizeObserver((entries) => {
    setRowWidth(entries[0]?.contentRect.width ?? null);
  });
  observer.observe(rowRef.current);
  return () => observer.disconnect();
}, []);

const pinnedCount = tabs.filter((tab) => tab.pinned).length;
const unpinnedCount = tabs.length - pinnedCount;
const estimatedTabWidth =
  rowWidth === null || unpinnedCount === 0
    ? null
    : (rowWidth - 42 * pinnedCount - 30) / unpinnedCount;
const hideCloseSlot = estimatedTabWidth !== null && estimatedTabWidth < 96;
const hideBadge = estimatedTabWidth !== null && estimatedTabWidth < 66;
```

Unpinned tabs skip rendering the close slot when `hideCloseSlot` and the badge when
`hideBadge`. In jsdom there is no `ResizeObserver`, so both flags stay off and existing
tests are unaffected.

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx
git commit -m "feat(spur-notebook): strip widths width-lock pinned attention gestures"
```

---

### Task 3: Page tab actions: reopen, batch close, keyboard upgrades

**Task ID:** `task-page-actions`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/pages/tabActions.ts`
- Create: `crates/spur-notebook/jute-notebook/src/pages/tabActions.test.ts`
- Modify: `crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx`

**Depends on:** `task-tabs-store`

**Acceptance Criteria:**
- [ ] Pure helpers `closeOthersTargets`, `closeRightTargets`, `cycleTabId`, `jumpTabId` exist
      and are unit-tested (pinned exclusion, ⌘9 = last)
- [ ] `closeTab` pushes `{tab, index}` to the store's closed stack before removal
- [ ] `reopenClosedTab` reopens the popped path via `daemonControl({command:"open", activate:false})`,
      re-adds the tab, and restores its old index via `moveTab`
- [ ] Keyboard: ⌘⇧T reopen, ⌘9 last tab, ⌃Tab / ⌃⇧Tab cycle, ⌘W skips pinned tabs,
      existing ⌘T / ⌘1-8 / ⌘⌥←→ behavior preserved
- [ ] Batch close (`closeMany`) closes a list of ids with ONE confirm if any target is
      dirty or running
- [ ] `scripts/spur-pnpm test -- src/pages/tabActions.test.ts` passes and
      `scripts/spur-pnpm run typecheck` is clean

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files plus `NotebookPage.tsx` (keyboard effect, close/reopen
  callbacks, pendingClose generalization to a list).
- OUT of scope: `NotebookTabStrip.tsx`, stores beyond consuming Task 1 actions, route helpers.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```ts
// crates/spur-notebook/jute-notebook/src/pages/tabActions.test.ts
import { describe, expect, it } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import {
  closeOthersTargets,
  closeRightTargets,
  cycleTabId,
  jumpTabId,
} from "@/pages/tabActions";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: id,
  dirty: false,
  kernelState: "idle",
  mode: "cells",
  ...patch,
});

const tabs = [tab("p", { pinned: true }), tab("a"), tab("b"), tab("c")];

it("close others keeps the target and all pinned tabs", () => {
  expect(closeOthersTargets(tabs, "b")).toEqual(["a", "c"]);
});

it("close to the right excludes pinned tabs", () => {
  expect(closeRightTargets(tabs, "a")).toEqual(["b", "c"]);
  expect(closeRightTargets(tabs, "c")).toEqual([]);
});

it("cycles forward and backward with wrap-around", () => {
  expect(cycleTabId(tabs, "c", 1)).toBe("p");
  expect(cycleTabId(tabs, "p", -1)).toBe("c");
});

it("jumps 1-8 by index and 9 to the last tab", () => {
  expect(jumpTabId(tabs, 1)).toBe("p");
  expect(jumpTabId(tabs, 9)).toBe("c");
  expect(jumpTabId(tabs, 8)).toBeUndefined();
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/pages/tabActions.test.ts`
Expected: FAIL (module does not exist)

- [ ] **Step 3: Implement the helpers**

```ts
// crates/spur-notebook/jute-notebook/src/pages/tabActions.ts
import type { NotebookTab } from "@/stores/notebook";

export function closeOthersTargets(
  tabs: readonly NotebookTab[],
  keepId: string,
): string[] {
  return tabs
    .filter((tab) => tab.id !== keepId && !tab.pinned)
    .map((tab) => tab.id);
}

export function closeRightTargets(
  tabs: readonly NotebookTab[],
  fromId: string,
): string[] {
  const index = tabs.findIndex((tab) => tab.id === fromId);
  if (index < 0) return [];
  return tabs
    .slice(index + 1)
    .filter((tab) => !tab.pinned)
    .map((tab) => tab.id);
}

export function cycleTabId(
  tabs: readonly NotebookTab[],
  activeTabId: string | undefined,
  offset: number,
): string | undefined {
  if (tabs.length === 0) return undefined;
  const index = Math.max(
    0,
    tabs.findIndex((tab) => tab.id === activeTabId),
  );
  return tabs[(index + offset + tabs.length) % tabs.length]?.id;
}

export function jumpTabId(
  tabs: readonly NotebookTab[],
  digit: number,
): string | undefined {
  if (digit === 9) return tabs[tabs.length - 1]?.id;
  return tabs[digit - 1]?.id;
}
```

- [ ] **Step 4: Wire `NotebookPage.tsx`**

1. Pull `pushClosedTab`, `popClosedTab`, `moveTab` from `useNotebookTabsStore`.
2. In `closeTab`, before splicing, record the index:

```ts
const removedIndex = tabs.findIndex((candidate) => candidate.id === tabId);
if (tab.path) pushClosedTab({ tab, index: removedIndex });
```

3. Add `reopenClosedTab`:

```ts
const reopenClosedTab = useCallback(async () => {
  const record = popClosedTab();
  if (!record?.tab.path) return;
  try {
    const response = await daemonControl({
      command: "open",
      path: record.tab.path,
      activate: false,
    });
    addOrFocusNotebookPath(pathFromDaemonControlResponse(response, "open"));
    moveTab(record.tab.path, record.index);
  } catch (caught) {
    setTabError(errorMessage(caught));
  }
}, [addOrFocusNotebookPath, moveTab, popClosedTab]);
```

4. Generalize `pendingCloseTabId: string | null` to `pendingCloseIds: string[] | null`.
   `confirmCloseNeeded` is true when ANY pending id is dirty/running (reuse
   `tabRequiresCloseConfirmation` per id). `closeMany(ids)` sets `pendingCloseIds`; the
   existing auto-close effect and `ConfirmModal` confirm loop over the list calling
   `closeTab` sequentially. Title for multi: `Close ${ids.length} tabs?`; body:
   `"${riskyCount} of these tabs have unsaved changes or running kernels. Closing tears down their kernel slots."`
5. Replace the keyboard effect:

```ts
const onKeyDown = (event: KeyboardEvent) => {
  if (event.key === "Tab" && event.ctrlKey) {
    event.preventDefault();
    const next = cycleTabId(tabs, activeTabId, event.shiftKey ? -1 : 1);
    if (next) setActiveTabId(next);
    return;
  }
  if (!event.metaKey) return;
  const key = event.key.toLowerCase();
  if (key === "t" && event.shiftKey) {
    event.preventDefault();
    void reopenClosedTab();
    return;
  }
  if (key === "t") {
    event.preventDefault();
    void addTab();
    return;
  }
  if (key === "w") {
    event.preventDefault();
    const tab = tabs.find((candidate) => candidate.id === activeTabId);
    if (tab && !tab.pinned) requestCloseTab(tab.id);
    return;
  }
  if (/^[1-9]$/.test(key)) {
    event.preventDefault();
    const target = jumpTabId(tabs, Number(key));
    if (target) setActiveTabId(target);
    return;
  }
  if (event.altKey && (key === "arrowleft" || key === "arrowright")) {
    event.preventDefault();
    const next = cycleTabId(tabs, activeTabId, key === "arrowleft" ? -1 : 1);
    if (next) setActiveTabId(next);
  }
};
```

- [ ] **Step 5: Run tests + typecheck**

Run: `scripts/spur-pnpm test -- src/pages/tabActions.test.ts` then
`scripts/spur-pnpm run typecheck`
Expected: PASS / clean

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/pages/tabActions.ts crates/spur-notebook/jute-notebook/src/pages/tabActions.test.ts crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx
git commit -m "feat(spur-notebook): reopen batch-close and keyboard tab actions"
```

---

### Task 4: TabHoverCard component

**Task ID:** `task-hover-card`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.tsx`
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.test.tsx`

**Depends on:** `task-tabs-store`

**Acceptance Criteria:**
- [ ] Renders filename, path, kernel line, resources line, mode, unsaved state
- [ ] Kernel line: `none` when `kernelState === "idle"`, otherwise `<state> · gen <n>` using
      `kernelGeneration`
- [ ] Resources line shows `"·"` when `stats` is null
- [ ] Exposes `useTabHoverDelay(delayMs)` hook: returns `{hoveredTabId, anchor, onTabEnter, onTabLeave, cancel}`
      where `onTabEnter(tabId, rect)` arms a 350ms timer and `onTabLeave`/`cancel` clears it
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/TabHoverCard.test.tsx` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files only. Purely presentational; no store access, no Tauri calls.
- OUT of scope: `NotebookTabStrip.tsx` wiring (Task 8 does that).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.test.tsx
import { act, render, renderHook, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabHoverCard, { useTabHoverDelay } from "@/ui/notebook/TabHoverCard";

const tab: NotebookTab = {
  id: "~/notebooks/etl.ipynb",
  path: "~/notebooks/etl.ipynb",
  title: "etl.ipynb",
  dirty: false,
  kernelState: "running",
  kernelGeneration: 2,
  mode: "cells",
};

it("renders kernel and resource lines", () => {
  render(
    <TabHoverCard
      anchor={{ left: 10, bottom: 40 }}
      stats={{
        kernel_id: "k",
        spec_name: "python3",
        generation: 2,
        status: "alive",
        cpu_pct: 64.2,
        mem_mb: 1212,
      }}
      tab={tab}
    />,
  );
  expect(screen.getByText("running · gen 2")).toBeInTheDocument();
  expect(screen.getByText("64% CPU · 1212 MB")).toBeInTheDocument();
});

it("shows placeholder resources without stats and none for idle kernels", () => {
  render(
    <TabHoverCard
      anchor={{ left: 0, bottom: 0 }}
      stats={null}
      tab={{ ...tab, kernelState: "idle", kernelGeneration: undefined }}
    />,
  );
  expect(screen.getByText("none")).toBeInTheDocument();
  expect(screen.getByText("·")).toBeInTheDocument();
});

it("arms the hover card only after the delay", () => {
  vi.useFakeTimers();
  const { result } = renderHook(() => useTabHoverDelay(350));
  act(() => {
    result.current.onTabEnter("t1", { left: 5, bottom: 30 });
  });
  expect(result.current.hoveredTabId).toBeUndefined();
  act(() => {
    vi.advanceTimersByTime(350);
  });
  expect(result.current.hoveredTabId).toBe("t1");
  act(() => {
    result.current.onTabLeave();
  });
  expect(result.current.hoveredTabId).toBeUndefined();
  vi.useRealTimers();
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabHoverCard.test.tsx`
Expected: FAIL (module does not exist)

- [ ] **Step 3: Implement**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.tsx
import { useCallback, useRef, useState } from "react";

import type { KernelSlotInfo, NotebookTab } from "@/stores/notebook";

export type HoverAnchor = { left: number; bottom: number };

type Props = {
  anchor: HoverAnchor;
  stats: KernelSlotInfo | null;
  tab: NotebookTab;
};

export default function TabHoverCard({ anchor, stats, tab }: Props) {
  const kernelLine =
    tab.kernelState === "idle"
      ? "none"
      : `${tab.kernelState} · gen ${tab.kernelGeneration ?? 0}`;
  const resourceLine = stats
    ? `${Math.round(stats.cpu_pct)}% CPU · ${Math.round(stats.mem_mb)} MB`
    : "·";

  return (
    <div
      className="fixed z-50 w-64 rounded-lg border border-gray-200 bg-white p-3 shadow-lg"
      role="tooltip"
      style={{ left: anchor.left, top: anchor.bottom + 8 }}
    >
      <div className="text-xs font-semibold text-gray-900">{tab.title}</div>
      {tab.path && (
        <div className="break-all font-mono text-[10px] text-gray-400">
          {tab.path}
        </div>
      )}
      <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 border-t border-gray-200 pt-2 text-[11px]">
        <dt className="text-gray-400">Kernel</dt>
        <dd className="font-mono text-gray-900">{kernelLine}</dd>
        <dt className="text-gray-400">Resources</dt>
        <dd className="font-mono text-gray-900">{resourceLine}</dd>
        <dt className="text-gray-400">Mode</dt>
        <dd className="font-mono text-gray-900">{tab.mode}</dd>
        <dt className="text-gray-400">Unsaved</dt>
        <dd className="font-mono text-gray-900">{tab.dirty ? "yes" : "no"}</dd>
      </dl>
    </div>
  );
}

export function useTabHoverDelay(delayMs: number) {
  const [hoveredTabId, setHoveredTabId] = useState<string | undefined>();
  const [anchor, setAnchor] = useState<HoverAnchor>({ left: 0, bottom: 0 });
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancel = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setHoveredTabId(undefined);
  }, []);

  const onTabEnter = useCallback(
    (tabId: string, rect: HoverAnchor) => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        setAnchor(rect);
        setHoveredTabId(tabId);
      }, delayMs);
    },
    [delayMs],
  );

  return { anchor, cancel, hoveredTabId, onTabEnter, onTabLeave: cancel };
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabHoverCard.test.tsx`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/TabHoverCard.test.tsx
git commit -m "feat(spur-notebook): tab hover card with kernel stats"
```

---

### Task 5: TabContextMenu component

**Task ID:** `task-context-menu`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.tsx`
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.test.tsx`

**Depends on:** `task-tabs-store`

**Acceptance Criteria:**
- [ ] Items in order: Pin/Unpin, Close (⌘W hint), Close Others (n), Close to the Right (n),
      Reopen Closed Tab (⌘⇧T hint), Copy Path; disabled stubs: Duplicate, Move to New Window
- [ ] Pin item reads "Unpin tab" for pinned tabs; Close shows no ⌘W hint for pinned tabs
- [ ] Close Others / Close to the Right disabled when their count is 0; Reopen disabled when
      `canReopen` is false; Copy Path disabled when the tab has no path
- [ ] Clicking an enabled item invokes its callback and then `onDismiss`
- [ ] `mousedown` outside the menu invokes `onDismiss`
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/TabContextMenu.test.tsx` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files only. Purely presentational; callbacks in, no store access.
- OUT of scope: strip wiring (Task 8), kernel verbs (deferred per spec §4).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.test.tsx
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabContextMenu from "@/ui/notebook/TabContextMenu";

const tab: NotebookTab = {
  id: "a",
  path: "~/notebooks/a.ipynb",
  title: "a.ipynb",
  dirty: false,
  kernelState: "live",
  mode: "cells",
};

function renderMenu(overrides = {}) {
  const props = {
    canReopen: true,
    closeOthersCount: 2,
    closeRightCount: 0,
    onClose: vi.fn(),
    onCloseOthers: vi.fn(),
    onCloseRight: vi.fn(),
    onCopyPath: vi.fn(),
    onDismiss: vi.fn(),
    onReopenClosed: vi.fn(),
    onTogglePin: vi.fn(),
    position: { x: 10, y: 10 },
    tab,
    ...overrides,
  };
  render(<TabContextMenu {...props} />);
  return props;
}

it("invokes close-others and dismisses", () => {
  const props = renderMenu();
  fireEvent.click(screen.getByRole("menuitem", { name: /Close others \(2\)/ }));
  expect(props.onCloseOthers).toHaveBeenCalled();
  expect(props.onDismiss).toHaveBeenCalled();
});

it("disables zero-count and unavailable items", () => {
  renderMenu({ canReopen: false });
  expect(
    screen.getByRole("menuitem", { name: /Close to the right/ }),
  ).toBeDisabled();
  expect(
    screen.getByRole("menuitem", { name: /Reopen closed tab/ }),
  ).toBeDisabled();
  expect(
    screen.getByRole("menuitem", { name: /Move to new window/ }),
  ).toBeDisabled();
});

it("shows Unpin for pinned tabs", () => {
  renderMenu({ tab: { ...tab, pinned: true } });
  expect(screen.getByRole("menuitem", { name: "Unpin tab" })).toBeVisible();
});

it("dismisses on outside mousedown", () => {
  const props = renderMenu();
  fireEvent.mouseDown(document.body);
  expect(props.onDismiss).toHaveBeenCalled();
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabContextMenu.test.tsx`
Expected: FAIL (module does not exist)

- [ ] **Step 3: Implement**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.tsx
import { useEffect, useRef } from "react";

import type { NotebookTab } from "@/stores/notebook";

type Props = {
  canReopen: boolean;
  closeOthersCount: number;
  closeRightCount: number;
  onClose: () => void;
  onCloseOthers: () => void;
  onCloseRight: () => void;
  onCopyPath: () => void;
  onDismiss: () => void;
  onReopenClosed: () => void;
  onTogglePin: () => void;
  position: { x: number; y: number };
  tab: NotebookTab;
};

export default function TabContextMenu(props: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onMouseDown = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) props.onDismiss();
    };
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  });

  const item = (
    label: string,
    onClick: (() => void) | undefined,
    options: { disabled?: boolean; kbd?: string } = {},
  ) => (
    <button
      className="flex w-full items-center justify-between gap-4 rounded px-2.5 py-1.5 text-left text-xs text-gray-900 enabled:hover:bg-gray-100 disabled:text-gray-400"
      disabled={options.disabled || !onClick}
      onClick={() => {
        onClick?.();
        props.onDismiss();
      }}
      role="menuitem"
      type="button"
    >
      <span>{label}</span>
      {options.kbd && (
        <span className="font-mono text-[10px] text-gray-400">
          {options.kbd}
        </span>
      )}
    </button>
  );
  const sep = <div className="mx-1.5 my-1 h-px bg-gray-200" />;

  return (
    <div
      className="fixed z-50 min-w-[228px] rounded-lg border border-gray-200 bg-white p-1 shadow-lg"
      ref={ref}
      role="menu"
      style={{ left: props.position.x, top: props.position.y }}
    >
      {item(props.tab.pinned ? "Unpin tab" : "Pin tab", props.onTogglePin)}
      {item("Duplicate", undefined, { disabled: true })}
      {sep}
      {item("Close", props.onClose, {
        kbd: props.tab.pinned ? undefined : "⌘W",
      })}
      {item(`Close others (${props.closeOthersCount})`, props.onCloseOthers, {
        disabled: props.closeOthersCount === 0,
      })}
      {item(
        `Close to the right (${props.closeRightCount})`,
        props.onCloseRight,
        { disabled: props.closeRightCount === 0 },
      )}
      {item("Reopen closed tab", props.onReopenClosed, {
        disabled: !props.canReopen,
        kbd: "⌘⇧T",
      })}
      {sep}
      {item("Copy path", props.onCopyPath, { disabled: !props.tab.path })}
      {item("Move to new window", undefined, { disabled: true })}
    </div>
  );
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabContextMenu.test.tsx`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/TabContextMenu.test.tsx
git commit -m "feat(spur-notebook): tab context menu component"
```

---

### Task 6: TabListMenu (searchable ▾ panel)

**Task ID:** `task-tab-list-menu`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.tsx`
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.test.tsx`

**Depends on:** `task-tabs-store`

**Acceptance Criteria:**
- [ ] Search input filters rows by case-insensitive substring over `title + path`
- [ ] Each row: kernel dot (orange idle / green live / pulsing green running), title,
      violet dirty dot when dirty, mono path line, "current" marker on the active tab
- [ ] Clicking a row calls `onSelect(tabId)` then `onDismiss`
- [ ] Panel footer keeps "New notebook" and "Open notebook..." actions (calls `onNewTab` /
      `onOpenNotebook` then `onDismiss`)
- [ ] Outside `mousedown` dismisses
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/TabListMenu.test.tsx` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two new files only.
- OUT of scope: strip wiring (Task 8 replaces the old inline ▾ menu).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.test.tsx
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabListMenu from "@/ui/notebook/TabListMenu";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  path: `~/notebooks/${id}.ipynb`,
  title: `${id}.ipynb`,
  dirty: false,
  kernelState: "live",
  mode: "cells",
  ...patch,
});

function renderMenu(overrides = {}) {
  const props = {
    activeTabId: "etl",
    onDismiss: vi.fn(),
    onNewTab: vi.fn(),
    onOpenNotebook: vi.fn(),
    onSelect: vi.fn(),
    tabs: [tab("etl"), tab("sales"), tab("scratch", { dirty: true })],
    ...overrides,
  };
  render(<TabListMenu {...props} />);
  return props;
}

it("filters rows by search query", () => {
  renderMenu();
  fireEvent.change(screen.getByPlaceholderText(/Search tabs/), {
    target: { value: "sal" },
  });
  expect(screen.queryByText("etl.ipynb")).toBeNull();
  expect(screen.getByText("sales.ipynb")).toBeVisible();
});

it("selects a tab and dismisses", () => {
  const props = renderMenu();
  fireEvent.click(screen.getByText("sales.ipynb"));
  expect(props.onSelect).toHaveBeenCalledWith("sales");
  expect(props.onDismiss).toHaveBeenCalled();
});

it("marks the current tab", () => {
  renderMenu();
  expect(screen.getByText("current")).toBeVisible();
});

it("keeps new/open actions", () => {
  const props = renderMenu();
  fireEvent.click(screen.getByRole("menuitem", { name: "New notebook" }));
  expect(props.onNewTab).toHaveBeenCalled();
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabListMenu.test.tsx`
Expected: FAIL (module does not exist)

- [ ] **Step 3: Implement**

```tsx
// crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.tsx
import clsx from "clsx";
import { useEffect, useRef, useState } from "react";

import type { NotebookTab } from "@/stores/notebook";

type Props = {
  activeTabId?: string;
  onDismiss: () => void;
  onNewTab: () => void | Promise<void>;
  onOpenNotebook: () => void | Promise<void>;
  onSelect: (tabId: string) => void;
  tabs: NotebookTab[];
};

export default function TabListMenu({
  activeTabId,
  onDismiss,
  onNewTab,
  onOpenNotebook,
  onSelect,
  tabs,
}: Props) {
  const [query, setQuery] = useState("");
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onMouseDown = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) onDismiss();
    };
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  }, [onDismiss]);

  const visible = tabs.filter((tab) =>
    `${tab.title} ${tab.path ?? ""}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );

  return (
    <div
      className="absolute right-0 top-full z-20 mt-1 w-80 rounded-lg border border-gray-200 bg-white p-2 shadow-lg"
      ref={ref}
      role="menu"
    >
      <input
        autoFocus
        className="mb-1.5 w-full rounded border border-gray-200 px-2 py-1.5 text-xs text-gray-900 outline-none focus:border-gray-400"
        onChange={(event) => setQuery(event.target.value)}
        placeholder="Search tabs by name or path"
        value={query}
      />
      {visible.map((tab) => (
        <button
          className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-gray-100"
          key={tab.id}
          onClick={() => {
            onSelect(tab.id);
            onDismiss();
          }}
          role="menuitem"
          type="button"
        >
          <span
            className={clsx(
              "h-2 w-2 shrink-0 rounded-full",
              tab.kernelState === "idle" ? "bg-orange-500" : "bg-green-500",
              tab.kernelState === "running" && "animate-pulse",
            )}
          />
          <span className="min-w-0 flex-1">
            <span className="flex items-center gap-1.5 truncate text-xs text-gray-900">
              {tab.title}
              {tab.dirty && (
                <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-violet-500" />
              )}
            </span>
            <span className="block truncate font-mono text-[9px] text-gray-400">
              {tab.path ?? "scratch"}
            </span>
          </span>
          {tab.id === activeTabId && (
            <span className="shrink-0 font-mono text-[9px] text-violet-500">
              current
            </span>
          )}
        </button>
      ))}
      <div className="mx-1 my-1 h-px bg-gray-200" />
      <button
        className="w-full rounded px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100"
        onClick={() => {
          onDismiss();
          void onNewTab();
        }}
        role="menuitem"
        type="button"
      >
        New notebook
      </button>
      <button
        className="w-full rounded px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100"
        onClick={() => {
          onDismiss();
          void onOpenNotebook();
        }}
        role="menuitem"
        type="button"
      >
        Open notebook...
      </button>
    </div>
  );
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/TabListMenu.test.tsx`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/TabListMenu.test.tsx
git commit -m "feat(spur-notebook): searchable tab list menu"
```

---

### Task 7: Drag-to-reorder

**Task ID:** `task-drag-reorder`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx`

**Depends on:** `task-strip-anatomy`, `task-page-actions`

**Acceptance Criteria:**
- [ ] Non-pinned tabs are `draggable`; pinned tabs are not
- [ ] Drag-over shows a 2px `gray-900` drop indicator on the target's leading or trailing
      edge depending on pointer half
- [ ] Drop calls a new strip prop `onReorder(tabId, toIndex)`; index accounts for
      before/after half and is computed against the current `tabs` array
- [ ] `NotebookPage` handles `onReorder` with store `moveTab` + route sync:
      `setLocation(notebookRouteForPaths(orderedPaths, activePath))` using the post-move
      store order
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: drag/drop handlers in the strip, `onReorder` prop, the page handler.
- OUT of scope: pinned clamping logic (already in store `moveTab`), route helper signatures.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Add failing tests to `NotebookTabStrip.test.tsx`**

```tsx
it("pinned tabs are not draggable", () => {
  renderStrip([tab("pin", { pinned: true }), tab("a")], {
    onReorder: vi.fn(),
  });
  expect(screen.getByLabelText("pin.ipynb (pinned)")).toHaveAttribute(
    "draggable",
    "false",
  );
  expect(
    screen.getByRole("tab", { name: /a\.ipynb/ }).closest("[draggable]"),
  ).toHaveAttribute("draggable", "true");
});

it("emits onReorder with the drop index", () => {
  const onReorder = vi.fn();
  renderStrip([tab("a"), tab("b"), tab("c")], { onReorder });
  const tabA = screen.getByRole("tab", { name: /a\.ipynb/ }).closest("div")!;
  const tabC = screen.getByRole("tab", { name: /c\.ipynb/ }).closest("div")!;
  fireEvent.dragStart(tabA);
  fireEvent.dragOver(tabC, { clientX: 1000 });
  fireEvent.drop(tabC, { clientX: 1000 });
  expect(onReorder).toHaveBeenCalledWith("a", 2);
});
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx`
Expected: FAIL (`onReorder` prop unknown, draggable not set)

- [ ] **Step 3: Implement in the strip**

Add prop `onReorder: (tabId: string, toIndex: number) => void`. Component state:
`dragId: string | null`, `dropTarget: { id: string; before: boolean } | null`.

```tsx
draggable={!tab.pinned}
onDragStart={(event) => {
  if (tab.pinned) {
    event.preventDefault();
    return;
  }
  setDragId(tab.id);
  event.dataTransfer.effectAllowed = "move";
}}
onDragOver={(event) => {
  if (!dragId || dragId === tab.id || tab.pinned) return;
  event.preventDefault();
  const rect = event.currentTarget.getBoundingClientRect();
  setDropTarget({
    id: tab.id,
    before: event.clientX < rect.left + rect.width / 2,
  });
}}
onDrop={(event) => {
  event.preventDefault();
  if (!dragId || !dropTarget) return;
  const targetIndex = tabs.findIndex(
    (candidate) => candidate.id === dropTarget.id,
  );
  onReorder(dragId, targetIndex + (dropTarget.before ? 0 : 1));
  setDragId(null);
  setDropTarget(null);
}}
onDragEnd={() => {
  setDragId(null);
  setDropTarget(null);
}}
```

Indicator class on the target tab:
`dropTarget?.id === tab.id && (dropTarget.before ? "shadow-[-2px_0_0_0_#111827]" : "shadow-[2px_0_0_0_#111827]")`.
Drag source gets `opacity-50` while `dragId === tab.id`.

Note for jsdom: `getBoundingClientRect` returns zeros, so `clientX: 1000` lands in the
"after" half deterministically.

`onReorder` becomes a required strip prop: add `onReorder: vi.fn()` to the `renderStrip`
default props from Task 2 so the existing tests keep typechecking.

- [ ] **Step 4: Page handler**

```tsx
const reorderTab = useCallback(
  (tabId: string, toIndex: number) => {
    moveTab(tabId, toIndex);
    const ordered = useNotebookTabsStore.getState().tabs;
    const paths = ordered.flatMap((tab) => (tab.path ? [tab.path] : []));
    const activePath = ordered.find((tab) => tab.id === activeTabId)?.path;
    if (paths.length > 0 && activePath) {
      setLocation(notebookRouteForPaths(paths, activePath));
    }
  },
  [activeTabId, moveTab, setLocation],
);
```

Pass `onReorder={reorderTab}` to the strip. Import `notebookRouteForPaths` from
`./notebookRoute`.

- [ ] **Step 5: Run tests + typecheck, then commit**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx` and
`scripts/spur-pnpm run typecheck`

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx
git commit -m "feat(spur-notebook): drag to reorder tabs with route sync"
```

---

### Task 8: Integration: hover card, context menu, tab list wired into the strip

**Task ID:** `task-overlay-integration`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx`

**Depends on:** `task-hover-card`, `task-context-menu`, `task-tab-list-menu`, `task-drag-reorder`

**Acceptance Criteria:**
- [ ] Strip props extended with: `onCloseOthers(tabId)`, `onCloseRight(tabId)`,
      `onReopenClosed()`, `canReopen`, `onTogglePin(tabId)`,
      `getKernelStats(tabId): Promise<KernelSlotInfo | null>`
- [ ] Right-click on a tab opens `TabContextMenu` at the pointer; menu callbacks route to the
      page handlers; Copy Path writes `tab.path` via `navigator.clipboard.writeText` (try/catch)
- [ ] Hovering a tab for 350ms shows `TabHoverCard` anchored under the tab; entering a tab
      calls `getKernelStats` (only when `kernelState !== "idle"`), shows "·" until resolved;
      hover is suppressed while dragging or while a menu is open
- [ ] The old inline ▾ menu is replaced by `TabListMenu` (select switches tab; New/Open kept)
- [ ] `NotebookPage` supplies: `closeOthers`/`closeRight` via Task 3 helpers + `closeMany`,
      `reopenClosedTab`, `canReopen` from store `closedTabs.length > 0`, `togglePin` via store
      `setPinned`, and `getKernelStats` resolving the tab's `Notebook` from `tabEntries` and
      calling `entry.notebook.refreshKernelSlotInfo()` in try/catch returning null on error
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx` passes;
      `scripts/spur-pnpm run typecheck` clean

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three listed files.
- OUT of scope: the three overlay component files (consume as-is), stores, route helpers.
- If a component's props don't fit, emit `scope_drift` rather than editing the component.

**Implementation:**
- [ ] **Step 1: Add failing integration tests to `NotebookTabStrip.test.tsx`**

```tsx
it("opens the context menu on right-click and pins through it", () => {
  const onTogglePin = vi.fn();
  renderStrip([tab("a"), tab("b")], { onTogglePin });
  fireEvent.contextMenu(screen.getByRole("tab", { name: /b\.ipynb/ }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Pin tab" }));
  expect(onTogglePin).toHaveBeenCalledWith("b");
});

it("shows the hover card after 350ms with fetched stats", async () => {
  vi.useFakeTimers();
  const getKernelStats = vi.fn().mockResolvedValue({
    kernel_id: "k",
    spec_name: "python3",
    generation: 1,
    status: "alive",
    cpu_pct: 12,
    mem_mb: 96,
  });
  renderStrip([tab("a")], { getKernelStats });
  fireEvent.mouseEnter(screen.getByRole("tab", { name: /a\.ipynb/ }));
  await act(async () => {
    vi.advanceTimersByTime(350);
  });
  expect(screen.getByRole("tooltip")).toBeVisible();
  expect(getKernelStats).toHaveBeenCalledWith("a");
  vi.useRealTimers();
});

it("opens the searchable tab list from the overflow button", () => {
  const onSwitchTab = vi.fn();
  renderStrip([tab("a"), tab("b")], { onSwitchTab });
  fireEvent.click(screen.getByLabelText("Tab overflow"));
  fireEvent.click(screen.getByText("b.ipynb"));
  expect(onSwitchTab).toHaveBeenCalledWith("b");
});
```

(Update `renderStrip` defaults with the new required props:
`onReorder: vi.fn(), onCloseOthers: vi.fn(), onCloseRight: vi.fn(), onReopenClosed: vi.fn(),
canReopen: false, onTogglePin: vi.fn(), getKernelStats: vi.fn().mockResolvedValue(null)`.
Extend the test-file imports with `act` from `@testing-library/react`.)

- [ ] **Step 2: Run to verify failure, then implement strip wiring**

Strip state: `contextTarget: { tabId: string; x: number; y: number } | null`, list-menu open
boolean (reuse existing `menuOpen`), hover via `useTabHoverDelay(350)` plus
`stats: KernelSlotInfo | null` state. On tab `mouseEnter`: call `onTabEnter(tab.id, rect)`
from the hook with `rect = event.currentTarget.getBoundingClientRect()` mapped to
`{ left: rect.left, bottom: rect.bottom }`, and when `tab.kernelState !== "idle"` fire
`getKernelStats(tab.id).then(setStats)` (set null first). Suppress when `dragId` set or a
menu is open. On `contextMenu`: `event.preventDefault()`, set `contextTarget`.

Render at the strip root:

```tsx
{hoveredTabId && hoveredTab && !contextTarget && !menuOpen && (
  <TabHoverCard anchor={anchor} stats={stats} tab={hoveredTab} />
)}
{contextTarget && contextTab && (
  <TabContextMenu
    canReopen={canReopen}
    closeOthersCount={closeOthersCount(contextTab.id)}
    closeRightCount={closeRightCount(contextTab.id)}
    onClose={() => handleClose(contextTab.id)}
    onCloseOthers={() => onCloseOthers(contextTab.id)}
    onCloseRight={() => onCloseRight(contextTab.id)}
    onCopyPath={() => {
      if (contextTab.path) {
        void navigator.clipboard.writeText(contextTab.path).catch(() => {});
      }
    }}
    onDismiss={() => setContextTarget(null)}
    onReopenClosed={onReopenClosed}
    onTogglePin={() => onTogglePin(contextTab.id)}
    position={{ x: contextTarget.x, y: contextTarget.y }}
    tab={contextTab}
  />
)}
```

Counts inside the strip reuse Task 3 helpers:
`closeOthersTargets(tabs, id).length` / `closeRightTargets(tabs, id).length` (import from
`@/pages/tabActions`). Replace the old inline ▾ dropdown body with `<TabListMenu ... />`.

- [ ] **Step 3: Page wiring**

```tsx
const togglePin = useCallback(
  (tabId: string) => {
    const tab = useNotebookTabsStore.getState().tabs.find((t) => t.id === tabId);
    setPinned(tabId, !tab?.pinned);
  },
  [setPinned],
);

const getKernelStats = useCallback(
  async (tabId: string) => {
    const entry = tabEntries.find((candidate) => candidate.id === tabId);
    if (!entry) return null;
    try {
      return await entry.notebook.refreshKernelSlotInfo();
    } catch {
      return null;
    }
  },
  [tabEntries],
);

const closeOthers = useCallback(
  (tabId: string) => closeMany(closeOthersTargets(tabs, tabId)),
  [closeMany, tabs],
);
const closeRight = useCallback(
  (tabId: string) => closeMany(closeRightTargets(tabs, tabId)),
  [closeMany, tabs],
);
```

Pass all new props plus `canReopen={closedTabs.length > 0}` (subscribe:
`useNotebookTabsStore((state) => state.closedTabs.length > 0)`).

- [ ] **Step 4: Run tests + typecheck, then commit**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookTabStrip.test.tsx` and
`scripts/spur-pnpm run typecheck`

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookTabStrip.test.tsx crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx
git commit -m "feat(spur-notebook): wire hover card context menu and tab list"
```

---

### Task 9: Pinned-state persistence in the route

**Task ID:** `task-pinned-route`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/pages/notebookRoute.ts`
- Create: `crates/spur-notebook/jute-notebook/src/pages/notebookRoute.test.ts`
- Modify: `crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx`

**Depends on:** `task-overlay-integration`

**Acceptance Criteria:**
- [ ] `notebookRouteForPaths(paths, activePath?, pinnedPaths?)` appends one `pinned` query
      param per pinned path; `notebookRouteWithPath` forwards pinned paths unchanged
- [ ] New `pinnedPathsFromSearch(search): string[]` parses them back
- [ ] `tabsFromSearch` in `NotebookPage.tsx` sets `pinned: true` on tabs whose path is in the
      `pinned` params, and orders pinned tabs first
- [ ] `togglePin` and `reorderTab` update the location so pin state and order survive reload
- [ ] `scripts/spur-pnpm test -- src/pages/notebookRoute.test.ts` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three listed files.
- OUT of scope: stores, strip, daemon control.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

```ts
// crates/spur-notebook/jute-notebook/src/pages/notebookRoute.test.ts
import { describe, expect, it } from "vitest";

import {
  activeTabIdFromSearch,
  notebookRouteForPaths,
  pinnedPathsFromSearch,
} from "@/pages/notebookRoute";

it("serializes pinned paths as repeated params", () => {
  const route = notebookRouteForPaths(["/a.ipynb", "/b.ipynb"], "/b.ipynb", [
    "/a.ipynb",
  ]);
  const search = route.slice(route.indexOf("?"));
  expect(pinnedPathsFromSearch(search)).toEqual(["/a.ipynb"]);
  expect(activeTabIdFromSearch(search)).toBe("/b.ipynb");
});

it("returns an empty list when nothing is pinned", () => {
  const route = notebookRouteForPaths(["/a.ipynb"], "/a.ipynb");
  expect(pinnedPathsFromSearch(route.slice(route.indexOf("?")))).toEqual([]);
});
```

- [ ] **Step 2: Run to verify failure, then implement**

```ts
export function notebookRouteForPaths(
  paths: readonly string[],
  activePath?: string,
  pinnedPaths: readonly string[] = [],
): string {
  const params = new URLSearchParams();
  for (const path of uniquePaths(paths)) {
    params.append("path", path);
  }
  for (const path of uniquePaths(pinnedPaths)) {
    params.append("pinned", path);
  }
  if (activePath) {
    params.set("active", activePath);
  }
  const query = params.toString();
  return query ? `/notebook?${query}` : "/notebook";
}

export function pinnedPathsFromSearch(search: string): string[] {
  return new URLSearchParams(search).getAll("pinned");
}
```

`notebookRouteWithPath` gains the same optional `pinnedPaths` parameter and forwards it.

- [ ] **Step 3: Page wiring**

In `tabsFromSearch`, read `params.getAll("pinned")`, set `pinned: true` on matching tab
specs, and stable-sort pinned specs ahead of unpinned ones. Every `setLocation` call site
(`addOrFocusNotebookPath`, `reorderTab`, `togglePin`) computes
`pinnedPaths = tabs.filter((t) => t.pinned && t.path).map((t) => t.path!)` from the current
store order and passes it through. `togglePin` now also calls `setLocation` after
`setPinned` using the post-toggle store state.

- [ ] **Step 4: Run tests + typecheck, then commit**

Run: `scripts/spur-pnpm test -- src/pages/notebookRoute.test.ts` and
`scripts/spur-pnpm run typecheck`

```bash
git add crates/spur-notebook/jute-notebook/src/pages/notebookRoute.ts crates/spur-notebook/jute-notebook/src/pages/notebookRoute.test.ts crates/spur-notebook/jute-notebook/src/pages/NotebookPage.tsx
git commit -m "feat(spur-notebook): persist pinned tabs in the route"
```

---

### Task 10: Verification sweep

**Task ID:** `task-verify-sweep`

**Files:**
- Modify (only if fixes are needed): files touched by Tasks 1-9

**Depends on:** `task-pinned-route`

**Acceptance Criteria:**
- [ ] `scripts/spur-pnpm run typecheck` clean
- [ ] `scripts/spur-pnpm test` (full suite) passes, including the pre-existing
      `NotebookHeader`, `NotebookCells`, `ChatPanel`, store, and delta suites
- [ ] No `console.error` warnings introduced by the new components in test output
- [ ] Any fix is minimal and stays within files already touched by this plan

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: minimal fixes to plan-touched files to make the sweep green.
- OUT of scope: refactors, new features, snapshot-test mass updates without reading the diff.
- If a pre-existing test fails for reasons unrelated to this plan, emit a `blocker` signal
  with the failing test name instead of force-fixing it.

**Implementation:**
- [ ] **Step 1:** Run `scripts/spur-pnpm run typecheck`; fix type fallout.
- [ ] **Step 2:** Run `scripts/spur-pnpm test`; fix test fallout (typical: `NotebookPage`
      prop additions breaking older render helpers; update those tests' prop fixtures).
- [ ] **Step 3: Commit**

```bash
git add -A crates/spur-notebook/jute-notebook/src
git commit -m "test(spur-notebook): green sweep for browser-grade tabs"
```

---

## Dependency DAG

```
task-tabs-store
 ├─→ task-strip-anatomy ──┐
 ├─→ task-page-actions ───┼─→ task-drag-reorder ─┐
 ├─→ task-hover-card ─────┼──────────────────────┼─→ task-overlay-integration → task-pinned-route → task-verify-sweep
 ├─→ task-context-menu ───┘                      │
 └─→ task-tab-list-menu ─────────────────────────┘
```

Five tasks run in parallel after Task 1.

## Deferred (documented in spec §4, NOT in this plan)

Context-menu kernel verbs (Restart/Shut Down), Reveal in Finder, Duplicate, tab tear-off,
background-agent ◎ decoupling, keep-warm on unpinned close.
