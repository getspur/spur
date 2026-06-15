# Notebook Mode UIUX Workbench Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Inline Open Design + MCTS UX review, 2026-06-15
**Design epic:** Pending submit_plan epic

**Goal:** Make notebook mode feel like a coherent workbench by moving cell-local actions into the cell canvas, making destructive actions safe, clarifying global notebook controls, and reducing command palette noise.

**Architecture:** Keep the existing notebook store and cell mutation model. This plan is intentionally frontend-scoped: it reorganizes existing actions around clearer UI zones without changing notebook persistence or daemon contracts.

**Tech Stack:** React, Zustand, CodeMirror, lucide-react, Vitest, Testing Library, Tailwind classes.

---

## File Structure Mapping

- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx`
  Owns the center cell canvas and should host cell-local actions: run, insert, convert, schedule, delete, clear output.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx`
  Covers cell-local action routing, safety flows, and accessible labels.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx`
  Owns notebook/kernel-scope controls and mode switching only.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx`
  Owns command-palette access to actions; should hide unavailable commands and expose the palette visibly.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookView.tsx`
  Owns view composition and can pass app availability context indirectly through store selectors.
- `crates/spur-notebook/jute-notebook/src/ui/shared/ConfirmModal.tsx`
  Existing shared confirmation surface for destructive actions if the delete task chooses confirmation over undo.

---

### Task 1: Cell-Local Action Rail

**Task ID:** `t1-cell-rail`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Cell actions no longer render in the absolute `right-[-200px]` rail.
- [ ] Each code cell exposes visible, accessible cell-local controls for run, insert below, change type/language, schedule, and delete.
- [ ] Output clear remains visually associated with the output area.
- [ ] Existing `notebook.insertCellAfter`, `notebook.setCellType`, `notebook.setCellCodeType`, `notebook.execute`, and `notebook.clearResult` behavior is preserved.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `NotebookCells.tsx`, `NotebookCells.test.tsx`.
- OUT of scope: notebook store mutation semantics, daemon commands, DAG/App mode rendering.
- If moving controls requires shared design primitives, emit `scope_drift` before creating new cross-notebook components.

**Implementation:**
- [ ] Replace the `Aside`/`AsideIconButton` pattern with a compact in-cell toolbar or gutter that sits inside the cell row.
- [ ] Add an inline run button for code cells that calls `notebook.execute(cellId)`.
- [ ] Preserve existing accessible labels such as `Insert cell below`, `Delete cell`, and `Clear cell output`.
- [ ] Update tests to assert actions are reachable by role/name and dispatch to the same notebook methods.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx
git commit -m "fix(spur-notebook): improve notebook cell action rail"
```

---

### Task 2: Safe Cell Deletion

**Task ID:** `t2-safe-delete`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx`
- Reference: `crates/spur-notebook/jute-notebook/src/ui/shared/ConfirmModal.tsx`

**Depends on:** `t1-cell-rail`

**Acceptance Criteria:**
- [ ] Deleting a cell is no longer a single accidental click.
- [ ] The user must either confirm deletion or gets an immediate undo affordance before the destructive operation is final.
- [ ] Keyboard and screen-reader users can complete or cancel the flow.
- [ ] The test verifies cancel does not call `notebook.deleteCell` and confirm/commit does call it once.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: destructive delete UX for notebook cells.
- OUT of scope: persistent undo stack, notebook file format changes, cross-tab recovery.
- If a true undo requires store-level deleted-cell snapshots, emit `scope_drift`; prefer confirmation for this task.

**Implementation:**
- [ ] Reuse `ConfirmModal` if it fits the local interaction, otherwise add a small local confirmation state in `NotebookCells.tsx`.
- [ ] Confirmation copy must name the action plainly: `Delete cell?`, `Delete`, `Cancel`.
- [ ] Keep the delete trigger accessible as `Delete cell`.
- [ ] Add focused tests for cancel and confirm.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx
git commit -m "fix(spur-notebook): require confirmation before deleting cells"
```

---

### Task 3: Header and Mode Clarity

**Task ID:** `t3-header-modes`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx`
- Reference: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Header icon buttons have `aria-label` and `title` for run selected cell, restart kernel, kernel stats, schedules, and settings.
- [ ] The view-mode switch communicates when App mode is unavailable for a non-app notebook.
- [ ] Clicking unavailable App mode does not put a regular notebook into an empty/confusing app view.
- [ ] Existing DAG toggle shortcut behavior remains unchanged.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/NotebookHeader.test.tsx` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: header labels, titles, disabled/unavailable mode behavior.
- OUT of scope: AppMode internals, app manifest detection backend, sidebar behavior.
- If current app availability cannot be inferred from `viewState.appOpenInfo`, emit `scope_drift`.

**Implementation:**
- [ ] Use `state.viewState.appOpenInfo` to determine app availability.
- [ ] Render App mode disabled, hidden, or explained when no app info exists. Prefer disabled with a clear title if layout stability matters.
- [ ] Add accessible labels to unlabeled header buttons.
- [ ] Update tests for labels and non-app App-mode behavior.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookHeader.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx
git commit -m "fix(spur-notebook): clarify notebook header modes"
```

---

### Task 4: Command Palette Rationalization

**Task ID:** `t4-cmd-palette`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx`
- Optional Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx`
- Optional Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx`

**Depends on:** `t3-header-modes`

**Acceptance Criteria:**
- [ ] The command palette no longer shows disabled placeholder actions for run-all, restart-and-run-all, move cell up/down, or Black formatting.
- [ ] A visible command-palette trigger is available in notebook mode, or the existing shortcut is surfaced through an accessible control title.
- [ ] Deck-specific commands remain searchable but are grouped after notebook execution/actions, not before core notebook actions.
- [ ] Existing publish Spur App behavior remains unchanged.
- [ ] Relevant command menu tests pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: command ordering, unavailable-command hiding, visible trigger.
- OUT of scope: implementing run-all, cell reordering, Black formatting, deck command behavior.
- If a visible trigger requires a shared header component refactor, emit `scope_drift`.

**Implementation:**
- [ ] Remove or conditionally hide disabled placeholder `Command.Item` entries.
- [ ] Keep working commands: run cell, interrupt, restart, present mode, deck AI commands, publish Spur App, change cell type.
- [ ] Add or expose a visible command-palette trigger in header only if it can be done without broad header layout churn.
- [ ] Update tests to assert hidden unavailable commands and preserved working commands.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx
git commit -m "fix(spur-notebook): rationalize notebook command palette"
```

---

### Task 5: Integrated Notebook Mode Verification

**Task ID:** `t5-ux-verify`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookView.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx`

**Depends on:** `t2-safe-delete`, `t4-cmd-palette`

**Acceptance Criteria:**
- [ ] A user can discover and execute the core notebook flow in tests: select cell, run cell, insert below, cancel delete, confirm delete, switch to DAG, and return to Notebook.
- [ ] TypeScript passes for the notebook frontend.
- [ ] The relevant notebook tests pass through `scripts/spur-pnpm`.
- [ ] No unrelated package lock or generated file churn is introduced.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: tests and small integration glue required by previous tasks.
- OUT of scope: new backend commands, new app packaging behavior, broad visual redesign beyond the accepted tasks.
- If implementation discovers missing store APIs for required behaviors, emit `blocked` with exact missing API.

**Implementation:**
- [ ] Add or adjust integration-level tests around the user journey.
- [ ] Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx src/ui/notebook/NotebookHeader.test.tsx src/ui/notebook/NotebookCommandMenu.test.tsx src/ui/notebook/NotebookView.test.tsx
scripts/spur-pnpm run typecheck
```

- [ ] Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookView.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx
git commit -m "test(spur-notebook): cover notebook mode core UX flow"
```
