# Polyglot Notebook — Cell Identity & Kernel-Routing UI Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-notebook-polyglot-cell-ui-design.md`
**Design board:** `Untitled78.ipynb` (rendered artifact cell)

**Goal:** Make per-cell language/kernel a first-class, visible, switchable identity in the
linear notebook cell view, with the `✦ AI` cell as one entry in the same system.

**Architecture:** A single `cellLanguage.ts` token module is the source of truth for all five
routing targets (Python · JavaScript · Rust · Go · AI/spur). The cell header renders a chip +
left accent bar from that token; the chip opens a switcher menu that writes `code_type`;
`CellInput` selects CodeMirror grammar from `codeType`. Backend-gated behaviors (LIVE cascade,
switching *into* an AI cell) are surfaced disabled.

**Tech Stack:** React, Zustand, Tailwind, CodeMirror 6, Vitest, ts-rs.

---

### Task 1: Regenerate the `CodeType` ts-rs binding (add `go`)

**Task ID:** `task-1`

**Files:**
- Modify (regenerate): `crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `CodeType.ts` reads `"python" | "javascript" | "rust" | "go"`.
- [ ] No unrelated binding files change (or, if they do, it is reported — pre-existing drift).
- [ ] Commit includes only `src/bindings/` changes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: running the export binary; committing the regenerated `src/bindings/CodeType.ts`.
- OUT of scope: editing any `.ts` binding by hand; touching Rust source.
- The canonical Rust enum already has `Go` (`backend/notebook.rs:251`); this is a pure regen.

**Implementation:**
- [ ] **Step 1: Run the binding exporter**

```bash
cd crates/spur-notebook/jute-notebook/src-tauri
cargo run --bin ts-rs-export
```

- [ ] **Step 2: Verify the regenerated binding**

```bash
cat ../src/bindings/CodeType.ts
# Expect: export type CodeType = "python" | "javascript" | "rust" | "go";
git -C ../../.. status --porcelain crates/spur-notebook/jute-notebook/src/bindings
```

If files other than `CodeType.ts` changed, list them in the completion note (pre-existing drift)
and restage only the intended binding(s).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts
git commit -m "chore(notebook): regenerate CodeType ts-rs binding (add go)"
```

---

### Task 2: Cell language token module

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.ts`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.test.ts`

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] Exports `CellLanguageId`, `CellLanguageToken`, `CELL_LANGUAGE_TOKENS`,
      `CODE_LANGUAGE_ORDER`, `cellLanguageId(cell)`, `cellLanguageToken(cell)`.
- [ ] `cellLanguageId` returns `"spur"` when `cellMetadataOther.kernelspec.name === "spur"`,
      else the cell's `codeType`, else `"python"`.
- [ ] `pnpm run typecheck` and the new test pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the new module + its test only.
- OUT of scope: any change to `NotebookCells.tsx` / `CellInput.tsx` (later tasks consume this).
- If you need to touch consumer files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write the failing test**

```ts
// cellLanguage.test.ts
import { describe, expect, test } from "vitest";
import { cellLanguageId, cellLanguageToken } from "./cellLanguage";

describe("cellLanguage", () => {
  test("spur kernelspec wins regardless of codeType", () => {
    const cell = { cellMetadataOther: { kernelspec: { name: "spur" } }, codeType: "python" };
    expect(cellLanguageId(cell)).toBe("spur");
    expect(cellLanguageToken(cell).label).toBe("AI Agent");
  });
  test("falls back to codeType, then python", () => {
    expect(cellLanguageId({ codeType: "rust" })).toBe("rust");
    expect(cellLanguageId({})).toBe("python");
    expect(cellLanguageToken({ codeType: "go" }).glyph).toBe("Go");
  });
});
```

- [ ] **Step 2: Run it, watch it fail** — `pnpm vitest run src/ui/notebook/cellLanguage.test.ts`

- [ ] **Step 3: Implement the module**

```ts
import type { CodeType } from "@/bindings/CodeType";

export type CellLanguageId = CodeType | "spur";

export interface CellLanguageToken {
  id: CellLanguageId;
  label: string;
  glyph: string;
  kernelspec: string;
  accent: string;     // left accent bar + tinted gutter
  chipText: string;
  chipBg: string;
  chipBorder: string;
  glyphBg: string;
}

export const CELL_LANGUAGE_TOKENS: Record<CellLanguageId, CellLanguageToken> = {
  python:     { id: "python",     label: "Python",     glyph: "Py", kernelspec: "python3", accent: "#3776AB", chipText: "#2c5e8a", chipBg: "#ffffff", chipBorder: "#cfe0f1", glyphBg: "#eaf2fb" },
  javascript: { id: "javascript", label: "JavaScript", glyph: "JS", kernelspec: "deno",    accent: "#8A6D00", chipText: "#8a6d00", chipBg: "#ffffff", chipBorder: "#ead9a0", glyphBg: "#fcf7e8" },
  rust:       { id: "rust",       label: "Rust",       glyph: "Rs", kernelspec: "evcxr",   accent: "#CE422B", chipText: "#b23a22", chipBg: "#ffffff", chipBorder: "#e8b9ac", glyphBg: "#fbe9e4" },
  go:         { id: "go",         label: "Go",         glyph: "Go", kernelspec: "gonb",    accent: "#00ADD8", chipText: "#0a7e9e", chipBg: "#ffffff", chipBorder: "#a8deec", glyphBg: "#e5f6fb" },
  spur:       { id: "spur",       label: "AI Agent",   glyph: "✦",  kernelspec: "spur",    accent: "#7C3AED", chipText: "#6d28d9", chipBg: "#f5f3ff", chipBorder: "#ddd6fe", glyphBg: "#ffffff" },
};

export const CODE_LANGUAGE_ORDER: CellLanguageId[] = ["python", "javascript", "rust", "go", "spur"];

interface CellLike {
  codeType?: CodeType;
  cellMetadataOther?: Record<string, unknown>;
}

export function cellLanguageId(cell: CellLike): CellLanguageId {
  const ks = (cell.cellMetadataOther?.kernelspec as { name?: string } | undefined)?.name;
  if (ks === "spur") return "spur";
  return cell.codeType ?? "python";
}

export function cellLanguageToken(cell: CellLike): CellLanguageToken {
  return CELL_LANGUAGE_TOKENS[cellLanguageId(cell)];
}
```

- [ ] **Step 4: Run the test to green; run `pnpm run typecheck`.**
- [ ] **Step 5: Commit** — `feat(notebook): cell language token module`

---

### Task 3: Language chip, accent bar, tinted gutter in the cell view

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx`

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] Every code cell renders a language chip (glyph + label) sourced from `cellLanguageToken`.
- [ ] A 3px left accent bar in `token.accent` runs down each code cell.
- [ ] The gutter execution marker is tinted by `token.accent`; the AI cell keeps `✦[n]` and its
      existing `manual` / `● LIVE` pill.
- [ ] Markdown/raw cells render no chip/accent bar (unchanged).
- [ ] Existing compile-progress tests still pass; new chip tests pass; `pnpm run typecheck` clean.
- [ ] New test file is committed (`git add` + `git status --porcelain` shows clean).

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: `NotebookCells.tsx` + its test. Replace the existing `AiCellHeader` with a general
  `CellLanguageHeader` that delegates identity to `cellLanguage.ts`; keep the AI pill behavior.
- OUT of scope: the switcher menu (task-4) and CodeMirror highlighting (task-5). The chip may be
  a non-interactive `<span>` here; task-4 makes it a button.
- Do not change `isAiCell`/`cellAiLive` semantics — re-express them through the token module.

**Implementation:**
- [ ] **Step 1: Update the existing tests** to assert the chip label per language and the gutter
      accent. Reuse `createNotebookStoreForCell`. Add cases for a Python cell (chip "Python",
      no `✦`) and a Rust cell (chip "Rust"); keep the live/manual AI assertions.
- [ ] **Step 2: Replace `AiCellHeader` with `CellLanguageHeader`**

```tsx
import { cellLanguageId, cellLanguageToken } from "./cellLanguage";

function CellLanguageHeader({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const cell = useStore(notebook.store, (s) => s.serverState.cells[cellId]);
  if (!cell || cell.type !== "code") return null;
  const token = cellLanguageToken(cell);
  const isAi = cellLanguageId(cell) === "spur";
  const live = isAi && cellAiLive(cell);
  return (
    <div className="flex items-center gap-2 pl-[57px] pr-[18px] pt-3">
      <span
        className="inline-flex items-center gap-1.5 rounded border px-1.5 py-px font-mono text-[10px] font-semibold"
        style={{ color: token.chipText, background: token.chipBg, borderColor: token.chipBorder }}
      >
        <span
          className="inline-flex h-[18px] w-[18px] items-center justify-center rounded text-[9px]"
          style={{ background: token.glyphBg }}
        >
          {token.glyph}
        </span>
        {token.label}
      </span>
      {isAi && (
        <span
          className={clsx(
            "rounded border px-1.5 py-px font-mono text-[9px]",
            live ? "border-violet-600 bg-violet-600 text-white" : "border-gray-300 bg-white text-gray-500",
          )}
        >
          {live ? "● LIVE" : "manual"}
        </span>
      )}
    </div>
  );
}
```

- [ ] **Step 3: Accent bar + tinted gutter.** In the cell wrapper add an absolutely-positioned
      3px bar: `<span className="absolute left-0 top-4 bottom-4 w-[3px] rounded" style={{ background: token.accent }} />`
      for code cells. In `CellExecutionMarker`, replace the `ai ? "text-violet-600" : "text-gray-400"`
      branch with an inline `style={{ color: token.accent }}` for code cells (AI keeps the `✦` prefix).
- [ ] **Step 4:** Render `<CellLanguageHeader>` where `<AiCellHeader>` was (between
      `<CellInputAside>` and the `<Suspense>` editor).
- [ ] **Step 5:** `pnpm vitest run src/ui/notebook/NotebookCells.test.tsx` green; `pnpm run typecheck`.
- [ ] **Step 6: Commit** (stage the test file too) — `feat(notebook): per-cell language chip and accent`

---

### Task 4: Chip-as-switcher (kernel/type menu) + `setCellCodeType`

**Task ID:** `task-4`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/CellLanguageMenu.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/CellLanguageMenu.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx` (make chip a button)
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` (add `setCellCodeType`)

**Depends on:** task-2, task-3

**Acceptance Criteria:**
- [ ] Clicking the chip opens a menu: the four code languages + a divider + Markdown / Raw.
- [ ] Selecting a code language calls `notebook.setCellCodeType(cellId, id)` (and ensures the cell
      `type` is `"code"`); selecting Markdown/Raw calls `notebook.setCellType`.
- [ ] The **AI Agent** entry is rendered **disabled** with `title="Agent cells require backend wiring (bd-1bpb)"`.
- [ ] Menu closes on select / outside-click / Escape.
- [ ] `setCellCodeType` mirrors `setCellType`'s persistence path (local snapshot via
      `applyLocalCellSnapshot`, bumping `version`, writing `codeType`).
- [ ] New tests + `pnpm run typecheck` pass; new files committed.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: the menu component, the chip-button wiring in `NotebookCells.tsx`, and the
  `setCellCodeType` store method.
- OUT of scope: actually assigning the `spur` kernelspec (AI entry stays disabled); CodeMirror
  highlighting (task-5). Do not alter the daemon protocol.
- If persisting `code_type` requires a daemon control that `setCellType` does not use, match
  whatever `setCellType` does and emit `risk` if the local-only path looks insufficient.

**Implementation:**
- [ ] **Step 1: Add `setCellCodeType` to the notebook store**, mirroring `setCellType` (notebook.ts:1192):

```ts
setCellCodeType(cellId: string, codeType: CodeType) {
  const cell = selectCell(this.state, cellId);
  if (!cell || cell.codeType === codeType) return;
  this.applyLocalCellSnapshot(cellId, {
    ...cell,
    type: "code",
    codeType,
    version: cell.version + 1,
  });
}
```

- [ ] **Step 2: Write the failing test** for `CellLanguageMenu` — asserts the five rows render,
      AI Agent is `disabled`, selecting "Rust" calls `onSelectCodeType("rust")`, selecting
      "Markdown" calls `onSelectType("markdown")`.
- [ ] **Step 3: Implement `CellLanguageMenu.tsx`** driven by `CODE_LANGUAGE_ORDER` /
      `CELL_LANGUAGE_TOKENS`, with the open/selected styling from the design board (violet `sel`
      row, divider, Markdown/Raw rows). Close on outside-click + Escape.
- [ ] **Step 4: Wire the chip** in `CellLanguageHeader` to toggle the menu (button + `useState`
      open), routing code-language selections to `setCellCodeType` and Markdown/Raw to `setCellType`.
- [ ] **Step 5:** tests green; `pnpm run typecheck`.
- [ ] **Step 6: Commit** (stage new files) — `feat(notebook): cell language switcher menu`

---

### Task 5: Per-`code_type` CodeMirror highlighting

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/CellInput.tsx`
- Modify: `crates/spur-notebook/jute-notebook/package.json`

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `@codemirror/lang-javascript`, `@codemirror/lang-rust`, `@codemirror/legacy-modes` are deps.
- [ ] A code cell's editor uses the grammar for its `codeType`: python / javascript / rust /
      Go (via `StreamLanguage.define(go)`); unknown/spur → plain (no code grammar).
- [ ] Changing `codeType` reconfigures the `language` compartment live (no remount).
- [ ] `pnpm run typecheck` and `pnpm install` succeed; existing CellInput behavior for
      markdown/python is unchanged.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: `CellInput.tsx` language selection + the three deps in `package.json`.
- OUT of scope: the chip/switcher (tasks 3–4). Do not change unrelated editor extensions.
- If a CodeMirror language package version conflicts with the installed `@codemirror/*` (all
  `^6`), emit `risk` before pinning.

**Implementation:**
- [ ] **Step 1: Add deps** to `package.json`:

```
"@codemirror/lang-javascript": "^6.2.2",
"@codemirror/lang-rust": "^6.0.1",
"@codemirror/legacy-modes": "^6.4.0",
```

Run `pnpm install`.

- [ ] **Step 2: Read `codeType` in `CellInput`** from the store:

```ts
const codeType = useStore(notebook.store, (s) => s.serverState.cells[cellId].codeType);
```

- [ ] **Step 3: Rewrite `extensionForLanguage`** to take `(type, codeType)`:

```tsx
import { javascript } from "@codemirror/lang-javascript";
import { rust } from "@codemirror/lang-rust";
import { StreamLanguage } from "@codemirror/language";
import { go } from "@codemirror/legacy-modes/mode/go";

function extensionForLanguage(type: CellType, codeType?: CodeType): Extension {
  if (type === "markdown") return [markdown(), EditorView.lineWrapping];
  if (type !== "code") throw new Error(`Unsupported cell type: ${type}`);
  switch (codeType) {
    case "javascript": return javascript();
    case "rust":       return rust();
    case "go":         return StreamLanguage.define(go);
    case "python":
    default:           return python();
  }
}
```

- [ ] **Step 4:** Pass `codeType` at both the initial `language.of(...)` (line ~168) and the
      reconfigure `useEffect` (line ~199); add `codeType` to that effect's dependency array.
- [ ] **Step 5:** `pnpm run typecheck`; smoke a Rust and a Go cell in the editor.
- [ ] **Step 6: Commit** — `feat(notebook): code_type-driven editor highlighting`

---

## Dependency DAG

```
task-1 (regen binding) ── task-2 (token module) ── task-3 (chip/accent) ── task-4 (switcher)
        └───────────────── task-5 (highlighting)
```

- Roots: `task-1`.
- After task-1: `task-2` and `task-5` run in parallel.
- `task-3` after `task-2`; `task-4` after `task-3`.

## Self-review

- **Spec coverage:** token module (T2), chip+accent+gutter (T3), switcher (T4), highlighting (T5),
  Go binding (T1) — all four spec sections + the binding fix are covered.
- **Type consistency:** `CellLanguageId`, `CellLanguageToken`, `setCellCodeType(cellId, CodeType)`
  used consistently across T2→T4. `extensionForLanguage(type, codeType)` matches T5.
- **DAG:** acyclic; wide (T2/T5 parallel).
- **Backend-gated** items (LIVE cascade, switch-into-AI, agent/usage/cached) are explicitly OUT of
  scope and surfaced disabled — pairs with `bd-1bpb` + the backend-surface epic.
