# AI Sidebar Context Lenses — Frontend Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-ai-sidebar-context-lenses-design.md` (§10 boundaries 1-2; §11 frontend tests)
**Companion spec:** `docs/superpowers/specs/2026-06-12-ai-sidebar-context-provider-design.md` (§9 turn-context extension)
**Design epic:** `bd-f1ab` (closed)

**Goal:** Build the React/TypeScript AI-sidebar lens UI on top of the already-landed Rust turn-framing backend: a lens model with a compact control, lens-aware empty-state/composer copy, and a turn-context payload (`viewMode` + `lens` + optional `selectedCellRef`) sent to the existing `chat_turn` command.

**Architecture:** Backend is done — `chat_turn` already accepts `context: Option<ChatTurnContext>` (`crates/spur-notebook/jute-notebook/src-tauri/src/chat_commands.rs:33`), and `ChatTurnContext`/`ChatLens`/`NotebookViewMode` + `lens_preamble` exist in `crates/spur-notebook/src/sidebar_chat/types.rs`. This epic is purely the frontend in `crates/spur-notebook/jute-notebook/`. The frontend store's `NotebookViewMode` is `"cells" | "dag" | "app"` (`src/stores/notebook.ts:111`), but the Rust enum serializes `"notebook" | "dag" | "app"` — the payload builder must map `cells → notebook`.

**Tech Stack:** React, TypeScript, Zustand (notebook store), Vitest + @testing-library/react, Tauri `invoke`.

**Build/test:** Always `scripts/spur-pnpm` (never bare pnpm). Run one test file: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx`. Typecheck: `scripts/spur-pnpm run typecheck`. Force local if VM down: `SPUR_REMOTE=0 scripts/spur-pnpm test ...`.

---

## Integration contract (read first — fixed for both tasks)

```ts
// Frontend lens vocabulary — string values MUST match the Rust serde reprs.
export type ChatLens =
  | "notebook_builder"   // ChatLens::NotebookBuilder  (snake_case)
  | "notebook_deep_dive"
  | "dag_ops"
  | "app_product";

// Payload viewMode strings MUST match Rust NotebookViewMode (lowercase): "notebook" | "dag" | "app".
// The store uses "cells"; map it: cells -> "notebook".
```

`chat_turn` payload shape the backend already accepts (camelCase via serde):

```ts
invoke("chat_turn", {
  agentName, notebookPath, prompt, onEvent,
  context: {
    notebookPath,
    viewMode: "notebook" | "dag" | "app",
    lens: ChatLens,
    selectedCellRef?: string, // "cell://<id>" when a cell is focused
  },
});
```

Lens defaulting (spec §4):

| store viewMode | payload viewMode | default lens | alternate |
|---|---|---|---|
| `cells` | `notebook` | `notebook_builder` | `notebook_deep_dive` |
| `dag` | `dag` | `dag_ops` | — |
| `app` (with appOpenInfo) | `app` | `app_product` | — |
| `app` (no appOpenInfo) | `app` | `notebook_deep_dive` (soften, spec §4) | — |

---

## Task 1: Lens types, defaultLensFor, and ChatPanel lens UI

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/lens.ts` (types + `defaultLensFor` + copy maps)
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/lens.test.ts`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx` (lens state, segmented control, indicator, lens-aware empty state + composer copy)
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx` (UI tests)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `lens.ts` exports `ChatLens`, the payload `LensViewMode = "notebook" | "dag" | "app"`, `mapViewMode(store: NotebookViewMode): LensViewMode` (`cells → notebook`), `defaultLensFor(viewMode, appOpenInfo)` per the table above, and `EMPTY_STATE_COPY` / `composerLensLabel(lens)` maps from spec §5.
- [ ] `lens.test.ts`: `defaultLensFor` returns the expected lens for every row of the table (incl. the app-without-appOpenInfo softening).
- [ ] ChatPanel holds `lens` state derived from `viewState.viewMode`; in notebook (`cells`) mode it renders a compact two-option segmented control (`Builder` / `Deep dive`); in `dag` mode a single `Operations` indicator; in `app` mode a single `Product` indicator (no toggle).
- [ ] The control is visually secondary to the scope label (not tab-styled) and slots into the header after the scope row.
- [ ] Empty-state heading/supporting copy and composer status line reflect the active lens (spec §5 tables).
- [ ] Changing `viewState.viewMode` resets a manual lens override to the new default (spec §8).
- [ ] ChatPanel tests: notebook mode renders both controls; dag mode renders the operations indicator and **no** notebook toggle; app mode renders the product indicator; changing lens updates empty-state/composer copy.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx` and `... lens.test.ts` green; `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the four files above; lens model + UI + copy + state-reset rule.
- OUT of scope: the `chat_turn` payload wiring (that is task-2 — do not modify the `invoke("chat_turn", …)` arguments here), session/new-session commands, the backend Rust crate, the notebook store (`stores/notebook.ts`) beyond reading existing fields.
- Do **not** emit a `scope_drift` signal. Lens model + control + copy are one cohesive UI unit; the store already exposes `viewState.viewMode` and `viewState.appOpenInfo`.

**Implementation:**

- [ ] **Step 1: Write `lens.ts`:**

```ts
import type { NotebookViewMode } from "@/stores/notebook";
import type { NotebookOpenInfo } from "@/stores/notebook";

export type ChatLens = "notebook_builder" | "notebook_deep_dive" | "dag_ops" | "app_product";
export type LensViewMode = "notebook" | "dag" | "app";

export function mapViewMode(mode: NotebookViewMode): LensViewMode {
  return mode === "cells" ? "notebook" : mode; // "dag"|"app" pass through
}

export function defaultLensFor(mode: NotebookViewMode, app?: NotebookOpenInfo): ChatLens {
  switch (mode) {
    case "dag": return "dag_ops";
    case "app": return app ? "app_product" : "notebook_deep_dive";
    case "cells":
    default: return "notebook_builder";
  }
}

export const EMPTY_STATE_COPY: Record<ChatLens, { heading: string; copy: string }> = {
  notebook_builder: { heading: "Build on this notebook", copy: "Ask for the next cell, a cleaner analysis path, or stronger explanation." },
  notebook_deep_dive: { heading: "Understand this notebook", copy: "Ask how the cells, outputs, and assumptions fit together." },
  dag_ops: { heading: "Operate this graph", copy: "Ask about failed nodes, stale dependencies, or recomputation order." },
  app_product: { heading: "Improve this app", copy: "Ask about workflow, UI quality, copy, or product behavior." },
};

export function composerLensLabel(lens: ChatLens): string {
  return ({ notebook_builder: "Builder", notebook_deep_dive: "Deep dive", dag_ops: "Operations", app_product: "Product" } as const)[lens] + " lens";
}
```

(If `NotebookOpenInfo` is not exported from `stores/notebook.ts`, export it there as part of this task — it is already a named type at `stores/notebook.ts:166`.)

- [ ] **Step 2: Write `lens.test.ts`** asserting the full `defaultLensFor` table (incl. `defaultLensFor("app", undefined) === "notebook_deep_dive"`) and `mapViewMode("cells") === "notebook"`.

- [ ] **Step 3: Run to verify lens.test fails** (module not yet complete), then passes after Step 1.

Run: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/lens.test.ts`

- [ ] **Step 4: Wire lens state into ChatPanel.tsx.** Read `viewMode` from the store alongside the existing `appOpenInfo`/`path` selector (`ChatPanel.tsx:91-94`):

```tsx
const [notebookPath, appOpenInfo, viewMode] = useStore(
  notebook.store,
  useShallow((s) => [s.viewState.path, s.viewState.appOpenInfo, s.viewState.viewMode]),
);
const [lensOverride, setLensOverride] = useState<ChatLens | null>(null);
const lens = lensOverride ?? defaultLensFor(viewMode, appOpenInfo);
useEffect(() => { setLensOverride(null); }, [viewMode]); // §8 reset on view change
```

- [ ] **Step 5: Render the control** in the header (after the scope row, ~`ChatPanel.tsx:381`). Notebook mode → two `<button>`s (`Builder`, `Deep dive`) calling `setLensOverride("notebook_builder"|"notebook_deep_dive")`, the active one styled selected. DAG/app mode → a single static indicator (`DAG: Operations` / `App: Product`). Keep it visually secondary (small, muted) — not tab-styled.

- [ ] **Step 6: Lens-aware copy.** Replace the hardcoded empty-state (`ChatPanel.tsx:428-440`) with `EMPTY_STATE_COPY[lens]`, and the composer status (`ChatPanel.tsx:123-125`) to append `composerLensLabel(lens)` (e.g. `Ready in analysis.ipynb — Builder lens`).

- [ ] **Step 7: ChatPanel tests.** Extend the existing mock store (which returns `viewState: { appOpenInfo, path }`) to include `viewMode`, then add tests: notebook mode shows `Builder`+`Deep dive`; dag mode shows operations indicator and queries `screen.queryByRole("button", { name: "Deep dive" })` is null; app mode shows product indicator; clicking `Deep dive` swaps empty-state heading to "Understand this notebook".

- [ ] **Step 8: Run + typecheck + commit.**

Run: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx src/ui/notebook/sidebar/lens.test.ts`
Run: `scripts/spur-pnpm run typecheck`

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/lens.ts crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/lens.test.ts crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "feat(spur-notebook): task-1 ai sidebar lens model and control"
```

---

## Task 2: Turn-context payload to chat_turn

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx` (build + send `context` in the `chat_turn` invoke)
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx` (payload assertion + no-leak assertion)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] The `sendPrompt` handler captures `viewMode` and `lens` at submit time (like it already captures `turnNotebookPath`/`turnAgentName`) and passes `context: { notebookPath, viewMode: mapViewMode(viewMode), lens, selectedCellRef? }` to `invoke("chat_turn", …)`.
- [ ] `selectedCellRef` is included as `cell://<selectedCellId>` when `viewState.selectedCellId` is set, omitted otherwise.
- [ ] `chat_new_session` / session-list invokes are **not** given lens/context (spec §10 boundary 2; §8 "lens changes do not call chat_new_session").
- [ ] Test: submitting a prompt calls `chat_turn` with `expect.objectContaining({ context: { notebookPath: "/tmp/revenue.ipynb", viewMode: "notebook", lens: "notebook_builder" } })`.
- [ ] Test: changing lens does not trigger a `chat_new_session` invoke.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx` green; `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the `chat_turn` invoke arguments in `ChatPanel.tsx` and the two tests.
- OUT of scope: the lens UI/model (task-1 owns it; consume its exports), session commands' signatures, the backend crate.
- Do **not** emit a `scope_drift` signal. This is a small, contained payload-wiring change.

**Implementation:**

- [ ] **Step 1: Write the failing payload test** in `ChatPanel.test.tsx` extending the existing "streams chat_turn events" test pattern (mock store now exposes `viewMode: "cells"` from task-1):

```tsx
await waitFor(() => {
  expect(tauriMocks.invoke).toHaveBeenCalledWith(
    "chat_turn",
    expect.objectContaining({
      notebookPath: "/tmp/revenue.ipynb",
      prompt: "Summarize the notebook",
      context: expect.objectContaining({ viewMode: "notebook", lens: "notebook_builder" }),
    }),
  );
});
```

Plus a test asserting `chat_new_session` was never called with a `context`/`lens` key.

- [ ] **Step 2: Run to verify it fails** (no `context` sent yet).

- [ ] **Step 3: Implement.** In `sendPrompt` (`ChatPanel.tsx:285-322`), capture `const turnViewMode = viewMode; const turnLens = lens; const turnSelected = selectedCellId;` at the top (alongside existing captures), then extend the invoke:

```tsx
await invoke("chat_turn", {
  agentName: turnAgentName,
  notebookPath: turnNotebookPath,
  prompt: trimmedPrompt,
  onEvent,
  context: {
    notebookPath: turnNotebookPath,
    viewMode: mapViewMode(turnViewMode),
    lens: turnLens,
    ...(turnSelected ? { selectedCellRef: `cell://${turnSelected}` } : {}),
  },
});
```

Read `selectedCellId` from the store selector (`viewState.selectedCellId`) if not already in scope.

- [ ] **Step 4: Run + typecheck + commit.**

Run: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx`
Run: `scripts/spur-pnpm run typecheck`

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.test.tsx
git commit -m "feat(spur-notebook): task-2 send lens turn context to chat_turn"
```

---

## Dependency DAG

```
task-1 (lens model + UI) ──> task-2 (turn-context payload)
```

Sequential: task-2 consumes `mapViewMode`/`ChatLens` and the `lens` state introduced by task-1.

## Self-Review notes

- **Spec coverage:** lenses §10 boundary 1 → task-1; boundary 2 → task-2; boundary 3 (backend framing) already landed; provider §9 `selectedCellRef` → task-2.
- **String-value parity:** `ChatLens` reprs match Rust snake_case; payload `viewMode` maps `cells → notebook` to match Rust lowercase enum — the single most likely integration bug, called out explicitly and tested.
- **No new session per lens** (§8/§10) — enforced by a dedicated test.
- **DAG:** trivial 2-node chain.
