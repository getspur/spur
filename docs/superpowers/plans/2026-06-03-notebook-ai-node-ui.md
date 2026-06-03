# Notebook AI Node — DAG UI Rendering (Frontend Slice) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-notebook-ai-node-ui-design.ipynb`
**Design epic:** open-design board (committed to `main` at merge `7db4a39b`)

**Goal:** Render a `spur`-kernelspec **AI node** distinctly in the notebook DAG view — a violet AI-kind tag, the cell body shown as a quoted prompt (not mono code), a text-port glyph on the produced port, a manual/LIVE mode pill, and AI-aware inspector sections — using only data already reachable in the frontend, with backend-gated fields honestly deferred.

**Architecture:** Pure frontend, in `crates/spur-notebook/jute-notebook/src/ui/dag/`. An AI node is identified from data the store already carries: `cellMetadataOther.kernelspec.name === "spur"` (see `stores/notebook.ts:657`, where `const { spur, jute_deck, ...cellMetadataOther } = cell.metadata`). The reactive-DAG graph builder (`useDagGraph.ts`) gains a `kind` discriminator + `aiLive` flag; `DagNode` and `DagInspector` branch on `kind` to apply the AI treatment. The visual system is unchanged — status stays the only colour; AI-kind adds one restrained violet accent used at most twice per node.

**Tech Stack:** TypeScript, React, Zustand, clsx, Tailwind, Vitest + Testing Library (existing `*.test.tsx` / `*.test.ts` conventions in the same directory).

**Scope discipline (read before implementing):** This slice is **read-only rendering**. The Mode control is **display-only** (disabled, with a tooltip) because persisting `ai_live` needs a `daemonControl` command that does not exist yet, and live auto-run needs `bd-1bpb`. Agent name, token usage (`AiUsage`), the `cached` chip, and the `needs-agent` state are **not reachable in the frontend** and are NOT in scope — surfacing them is a separate backend-payload task (see Deferred). Do not invent values for them; do not add a no-op toggle that pretends to persist.

---

## File Structure Mapping

| File | Responsibility | Tasks |
|---|---|---|
| `ui/dag/useDagGraph.ts` | Add `DagNodeKind`, `kind`, `aiLive` to `DagNodeData`; derive them in `buildDagGraph`. | task-1 |
| `ui/dag/useDagGraph.test.ts` | Cover AI-kind + `aiLive` derivation. | task-1 |
| `ui/dag/DagNode.tsx` | Branch on `kind === "ai"`: AI tag, mode pill, quoted-prompt line, text-port glyph. | task-2 |
| `ui/dag/DagNode.test.tsx` | Cover AI-node rendering. | task-2 |
| `ui/dag/DagInspector.tsx` | AI-node header badge, disabled Mode control, "Prompt" section label. | task-3 |
| `ui/dag/DagInspector.test.tsx` (new) | Cover the AI inspector branch. | task-3 |

---

## Dependency DAG

```
task-1 (data: kind + aiLive)
   ├── task-2 (DagNode rendering)
   └── task-3 (DagInspector sections)
```

task-2 and task-3 are independent of each other and dispatch in parallel once task-1 is Approved.

---

### Task 1: `DagNodeData` gains `kind` + `aiLive`

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.ts`
- Test: `crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.test.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `DagNodeData` has a required `kind: DagNodeKind` (`"code" | "ai"`) and optional `aiLive?: boolean`.
- [ ] `buildDagGraph` sets `kind: "ai"` when `cell.cellMetadataOther?.kernelspec?.name === "spur"`, else `"code"`.
- [ ] `aiLive` reads `ai_live`/`aiLive` off `dagMetadata` defensively, defaulting to `false`.
- [ ] New tests pass; existing `useDagGraph.test.ts` cases still pass (update any inline `DagNodeData` fixtures to include `kind`).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `useDagGraph.ts` type + derivation, its test file.
- OUT of scope: `DagNode.tsx`, `DagInspector.tsx`, `stores/notebook.ts`, any Rust. If `cellMetadataOther` does not actually carry `kernelspec` at runtime, emit `scope_drift` rather than editing the store.

**Implementation:**

- [ ] **Step 1: Write the failing test** — append to `useDagGraph.test.ts`:

```ts
import { buildDagGraph } from "./useDagGraph";
import type { NotebookCellState } from "@/stores/notebook";

function aiCell(overrides: Partial<NotebookCellState> = {}): NotebookCellState {
  return {
    type: "code",
    initialText: "",
    source: "Summarise sales vs targets in 3 bullets.",
    version: 1,
    dagMetadata: { produces: [{ port: "summary", repr: "str" }], consumes: ["sales"] },
    cellMetadataOther: { kernelspec: { name: "spur" } },
    ...overrides,
  } as NotebookCellState;
}

it("marks a spur-kernelspec cell as an ai node", () => {
  const graph = buildDagGraph(["c1"], { c1: aiCell() });
  expect(graph.nodes[0].data.kind).toBe("ai");
});

it("defaults a normal code cell to kind=code", () => {
  const graph = buildDagGraph(["c1"], {
    c1: aiCell({ cellMetadataOther: undefined }),
  });
  expect(graph.nodes[0].data.kind).toBe("code");
});

it("derives aiLive from dag metadata ai_live", () => {
  const graph = buildDagGraph(["c1"], {
    c1: aiCell({
      dagMetadata: { produces: [{ port: "summary", repr: "str" }], consumes: ["sales"], ai_live: true } as never,
    }),
  });
  expect(graph.nodes[0].data.aiLive).toBe(true);
});
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/useDagGraph.test.ts`
Expected: FAIL (`kind` is not a property of `DagNodeData`).

- [ ] **Step 3: Implement** in `useDagGraph.ts`:

```ts
export type DagNodeKind = "code" | "ai";

// add to DagNodeData:
//   kind: DagNodeKind;
//   aiLive?: boolean;

function deriveNodeKind(cell: NotebookCellState): DagNodeKind {
  const name = (cell.cellMetadataOther?.kernelspec as { name?: string } | undefined)
    ?.name;
  return name === "spur" ? "ai" : "code";
}

function deriveAiLive(cell: NotebookCellState): boolean {
  const dag = cell.dagMetadata as
    | { ai_live?: boolean; aiLive?: boolean }
    | undefined;
  return Boolean(dag?.ai_live ?? dag?.aiLive);
}
```

In the `dagCellIds.map(...)` node builder add to the returned `data`:

```ts
          kind: deriveNodeKind(cell),
          aiLive: deriveAiLive(cell),
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/useDagGraph.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.ts \
        crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.test.ts
git commit -m "feat(notebook-ui): derive ai-node kind + aiLive in dag graph"
```

---

### Task 2: `DagNode` renders the AI-kind treatment

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.test.tsx`

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] When `data.kind === "ai"`: an `✦ AI` tag (violet `text-violet-700 bg-violet-50 border-violet-200`) renders under the header, followed by a `manual` / `LIVE` pill driven by `data.aiLive`.
- [ ] For an AI node the preview line shows the cell body as quoted roman text (`"` + `data.codePreview`), NOT the mono `codePreview` styling.
- [ ] The produced-port token for an AI node is prefixed with a `T` text-type glyph.
- [ ] Code nodes render exactly as before (no visual diff).
- [ ] The status rail/dot are unchanged for both kinds (status stays the only status colour).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `DagNode.tsx`, `DagNode.test.tsx`.
- OUT of scope: `useDagGraph.ts` (consume `kind`/`aiLive` only), `DagInspector.tsx`, agent-name/usage/cached (no data — do not render). If tempted to add an agent label, STOP: there is no agent data in the frontend; render the static `✦ AI` tag only.

**Implementation:**

- [ ] **Step 1: Write the failing test** — `DagNode.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import DagNode from "./DagNode";
import type { DagNodeData } from "./useDagGraph";

function data(overrides: Partial<DagNodeData> = {}): DagNodeData {
  return {
    id: "a3",
    label: "summary",
    cellType: "code",
    code: "Summarise sales vs targets.",
    codePreview: "Summarise sales vs targets.",
    produces: [{ port: "summary", repr: "str", version: 5 }],
    consumes: [{ port: "sales", version: 3 }],
    state: "fresh",
    kind: "ai",
    aiLive: true,
    ...overrides,
  };
}

it("renders the AI tag and LIVE pill for an ai node", () => {
  render(<DagNode data={data()} />);
  expect(screen.getByText(/AI/)).toBeInTheDocument();
  expect(screen.getByText(/LIVE/)).toBeInTheDocument();
});

it("renders manual pill when aiLive is false", () => {
  render(<DagNode data={data({ aiLive: false })} />);
  expect(screen.getByText(/manual/)).toBeInTheDocument();
});

it("does not render the AI tag for a code node", () => {
  render(<DagNode data={data({ kind: "code" })} />);
  expect(screen.queryByText(/✦/)).not.toBeInTheDocument();
});
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/DagNode.test.tsx`
Expected: FAIL (no AI tag rendered).

- [ ] **Step 3: Implement** — in `DagNode.tsx`, after the header `<div className="flex min-w-0 items-center gap-2">…</div>` block, add an AI sub-row and switch the preview line:

```tsx
{data.kind === "ai" && (
  <div className="mt-1.5 flex items-center gap-1.5">
    <span className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[9.5px] font-semibold text-violet-700">
      ✦ AI
    </span>
    <span
      className={clsx(
        "rounded border px-1.5 py-px font-mono text-[9px]",
        data.aiLive
          ? "border-violet-600 bg-violet-600 text-white"
          : "border-gray-300 bg-white text-gray-500",
      )}
    >
      {data.aiLive ? "● LIVE" : "manual"}
    </span>
  </div>
)}
```

Replace the existing preview `<p>` with a kind-aware version:

```tsx
{data.kind === "ai" ? (
  <p className="mt-1 line-clamp-2 text-[11px] leading-snug text-gray-700">
    <span className="font-semibold text-violet-600">“</span>
    {data.codePreview}
  </p>
) : (
  <p className="mt-1 truncate font-mono text-[10.5px] text-gray-500">
    {data.codePreview}
  </p>
)}
```

In `ProducedToken`, prefix the text glyph for AI nodes by passing a flag (or add a small `<span>` before the port name when the node is AI — keep it local to the produced-port map, e.g. render a `T` glyph span `className="rounded-sm border border-gray-300 px-0.5 font-mono text-[8px] text-gray-500"` before `data.produces.map(...)` tokens when `data.kind === "ai"`).

- [ ] **Step 4: Run the test, verify it passes**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/DagNode.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.test.tsx
git commit -m "feat(notebook-ui): render ai-node tag, mode pill, prompt + text port"
```

---

### Task 3: `DagInspector` AI-node sections

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.tsx`
- Test (create): `crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.test.tsx`

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] For an AI node (`node.kind === "ai"`): the header shows an `✦ AI` badge next to the status badge.
- [ ] A **Mode** section renders a `manual` / `live` segmented control that is **disabled** and carries `title="Live auto-run requires backend wiring (bd-1bpb)"`; the active segment reflects `node.aiLive`.
- [ ] The code section heading reads **"Prompt"** for an AI node and **"Code"** otherwise.
- [ ] Code nodes render exactly as before.
- [ ] No agent name, token usage, or cached UI is added (no data source).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `DagInspector.tsx`, new `DagInspector.test.tsx`.
- OUT of scope: wiring a real `ai_live` write (no `daemonControl` command exists — do NOT add one here), `useDagGraph.ts`, `DagNode.tsx`. The Mode control is display-only this slice. If a persisting toggle seems required, emit `scope_drift` — it is a backend task.

**Implementation:**

- [ ] **Step 1: Write the failing test** — `DagInspector.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import DagInspector from "./DagInspector";
import type { DagNodeData } from "./useDagGraph";

function node(overrides: Partial<DagNodeData> = {}): DagNodeData {
  return {
    id: "a3",
    label: "summary",
    cellType: "code",
    code: "Summarise sales vs targets.",
    codePreview: "Summarise sales vs targets.",
    produces: [{ port: "summary", repr: "str", version: 5 }],
    consumes: [{ port: "sales", version: 3 }],
    state: "fresh",
    kind: "ai",
    aiLive: false,
    ...overrides,
  };
}

it("shows the AI badge, Mode control, and Prompt heading for an ai node", () => {
  render(<DagInspector node={node()} portManifest={{}} />);
  expect(screen.getByText("AI", { exact: false })).toBeInTheDocument();
  expect(screen.getByText(/Mode/)).toBeInTheDocument();
  expect(screen.getByText(/Prompt/)).toBeInTheDocument();
});

it("labels the section Code for a code node", () => {
  render(<DagInspector node={node({ kind: "code" })} portManifest={{}} />);
  expect(screen.getByText(/Code/)).toBeInTheDocument();
  expect(screen.queryByText(/Mode/)).not.toBeInTheDocument();
});
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/DagInspector.test.tsx`
Expected: FAIL (no Mode/Prompt for AI node).

- [ ] **Step 3: Implement** — in `DagInspector.tsx`:

In the header `badges` area (next to the `status?.state ?? node.state` span) add:

```tsx
{node.kind === "ai" && (
  <span className="ml-2 inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-2 py-1 text-xs font-medium text-violet-700">
    ✦ AI
  </span>
)}
```

Before the Code `<section>`, add an AI-only Mode section:

```tsx
{node.kind === "ai" && (
  <section>
    <div className="mb-2 text-[11px] font-semibold uppercase tracking-normal text-gray-500">
      Mode
    </div>
    <div
      className="inline-flex overflow-hidden rounded border border-gray-300 font-mono text-xs opacity-60"
      title="Live auto-run requires backend wiring (bd-1bpb)"
    >
      <span className={clsx("px-3 py-1", !node.aiLive && "bg-violet-600 text-white")}>
        manual
      </span>
      <span className={clsx("px-3 py-1", node.aiLive && "bg-violet-600 text-white")}>
        live
      </span>
    </div>
  </section>
)}
```

Change the Code heading to be kind-aware:

```tsx
<div className="text-[11px] font-semibold uppercase tracking-normal text-gray-500">
  {node.kind === "ai" ? "Prompt" : "Code"}
</div>
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cd crates/spur-notebook/jute-notebook && pnpm vitest run src/ui/dag/DagInspector.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.test.tsx
git commit -m "feat(notebook-ui): ai-node inspector badge, mode display, prompt label"
```

---

## Deferred (NOT in this plan — backend-gated, would require inventing data)

These design elements have **no frontend data source** and must not be faked here:

| Design element | Why deferred | Where it belongs |
|---|---|---|
| Agent name on the `✦` tag / inspector Agent picker | `AgentConfig` is resolved in `ai_backend_from_config` (Rust); not in any frontend payload | new bead: surface resolved agent in `notebook_dag_status` |
| Token usage meter / `cached` chip | `AiUsage` lives in `AiRunOutput`; not threaded to `CellResult` | new bead: thread usage into the run result payload |
| `needs-agent` state | requires knowing whether an agent is configured (`NullAiBackend`) | new bead (same backend surface) |
| Persisting the Mode toggle + live auto-run | no `set_dag_metadata` `daemonControl` command; live cascade is AI-blind | **bd-1bpb** (wire AI backend into `spawn_reactive_engine`) + a `daemonControl` set-mode command |

**Recommendation:** after this slice merges, file one backend epic — "surface AI-node fields (agent, usage, needs-agent, ai_live persistence) to the notebook DAG status payload" — which unblocks turning the deferred items real.

---

## Self-Review

1. **Spec coverage:** Canvas/anatomy AI tag → task-2; prompt treatment → task-2; text-port glyph → task-2; mode pill → task-2 (display) + task-3 (inspector); state gallery `manual`/`live`/status states → task-1+task-2; inspector adaptations → task-3. `needs-agent`, agent name, usage, cached → explicitly Deferred (no data). ✅
2. **Placeholder scan:** all steps carry real code, real file paths, real `buildDagGraph`/`DagNodeData`/`DagInspectorProps` signatures, runnable vitest commands. No TBD. ✅
3. **Type consistency:** `DagNodeKind`/`kind`/`aiLive` defined in task-1 and consumed unchanged in task-2/task-3. ✅
4. **DAG validation:** task-1 root; task-2, task-3 depend only on task-1; no cycles; task-2 ∥ task-3. ✅
5. **beads compatibility:** each task has a unique id, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary with a `scope_drift` trigger. ✅
