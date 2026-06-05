# Notebook Delta Identity — Review-Hardening Plan

> **For SPUR orchestrator:** `submit_plan(persist_as_epic=true)`. Base = repo_main (the leak fix `c93b25b1`+`f589b196` is already merged).

**Source:** graph-grounded structural review of the merged cross-notebook leak fix (commits `c93b25b1`, `f589b196`).

**Goal:** Close the two safe, well-defined gaps the review surfaced — an unguarded delta-apply on the agent-bridge path + a typing hole, and missing test coverage — without expanding the merged fix.

**Architecture:** The merged fix stamps `NotebookDelta.path` (Rust) and drops foreign-path deltas in `reconcileNotebookDelta` (TS). This plan (a) routes the one remaining direct `applyNotebookDelta` call on the agent-bridge path through the guard and removes a `path` typing cast, and (b) adds the missing Rust + TS tests. Two tasks, file-isolated, run in parallel.

**Tech stack:** Rust (`jute`), TypeScript (Vitest).

---

## Out of scope (separate beads — do NOT touch in this plan)
- **Path canonicalization** (`make_delta` raw `display()` vs daemon `LoadNotebook` raw `PathBuf::from` vs frontend raw `setPath`): needs blast-radius design (canonicalizing the store path affects kernel-slot IDs and save paths). Tracked separately.
- **Agent-bridge broadcast** (`app.emit("agent://request")` to all windows) and **single-store `resyncFromSnapshot`**: the dormant Tauri-bridge leak and the DashMap-per-path follow-up. Tracked separately.

---

## Task 1: Guard the agent-bridge metadata apply + remove the path cast

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` (add `path?` to `DagStatusChangedDelta` ~111; drop the cast in `reconcileNotebookDelta` ~1014)
- Modify: `crates/spur-notebook/jute-notebook/src/agent/handlers.ts` (`setCellMetadata` ~215; import)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `DagStatusChangedDelta` has `path?: string`; the `(delta as { path?: string | null }).path` cast in `reconcileNotebookDelta` is replaced by a plain `delta.path` access.
- [ ] `handlers.ts::setCellMetadata` applies its daemon-response delta via `reconcileNotebookDelta(notebook, delta)` instead of `notebook.applyNotebookDelta(delta)`, so it honors the path guard.
- [ ] `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `notebook.ts` (the two edits), `handlers.ts` (`setCellMetadata` + its import).
- OUT: path canonicalization, `events.ts`, `bridge.ts`, Rust, bindings, tests (Task 2 owns `notebook.test.ts`).
- Do NOT emit `scope_drift` for minor in-scope decisions; emit `risk` only for a genuine blocker. Stay in scope.

**Implementation:**

- [ ] **Step 1:** In `notebook.ts`, add `path?: string` to `DagStatusChangedDelta`:

```ts
type DagStatusChangedDelta = {
  version: number;
  path?: string;
  kind: {
    type: "dagStatusChanged";
    snapshot: DagStatusSnapshot;
  };
};
```

- [ ] **Step 2:** In `reconcileNotebookDelta`, replace the cast with a direct access (now valid across the union since both members declare `path?`):

```ts
  if (!notebookDeltaIsForPath(notebook.state.viewState.path, delta.path)) {
    // Delta belongs to a different open notebook window; ignore it.
    return;
  }
```

- [ ] **Step 3:** In `handlers.ts`, import `reconcileNotebookDelta` and use it in `setCellMetadata` (replace line `notebook.applyNotebookDelta(delta);`):

```ts
import { type CellType, type Notebook, reconcileNotebookDelta, selectCell } from "@/stores/notebook";
```
```ts
  await reconcileNotebookDelta(notebook, delta);
  return { ok: true, version: delta.version };
```

- [ ] **Step 4:** Verify: `scripts/spur-pnpm run typecheck` (fall back to `SPUR_REMOTE=0` if remote pnpm hits the known `ENOTDIR node_modules` infra error, and say so). Maintain a clean worktree; commit exactly once: `fix(spur-notebook): route agent set_cell_metadata through path guard; type delta.path`.

---

## Task 2: Add the missing delta-identity tests

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` (`#[cfg(test)] mod tests`)
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.test.ts`

**Depends on:** none (runs in parallel with task-1; no shared files)

**Acceptance Criteria:**
- [ ] Rust test asserts `load` and `replace` stamp `delta.path` on the `Loaded` variant.
- [ ] TS test asserts `reconcileNotebookDelta` drops a foreign-path delta — neither `applyNotebookDelta` nor `resyncFromSnapshot` is invoked — and applies a matching-path delta.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute notebook_store` and `scripts/spur-pnpm test -- src/stores/notebook.test.ts` pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the two test files only.
- OUT: production code (Task 1 owns it), path canonicalization, bindings.
- Do NOT emit `scope_drift` for minor in-scope decisions; emit `risk` only for a genuine blocker.

**Implementation:**

- [ ] **Step 1:** Rust — add to the `notebook_store.rs` tests module (helpers `NotebookStore::new`, `SaveCoordinator::default`, `notebook_with_source` exist):

```rust
    #[test]
    fn loaded_delta_carries_path_on_load_and_replace() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        let load = store.load("/tmp/load.ipynb", notebook_with_source("a"));
        assert_eq!(load.path.as_deref(), Some("/tmp/load.ipynb"));
        assert!(matches!(load.kind, DeltaKind::Loaded { .. }));

        let replace = store.replace("/tmp/replace.ipynb", notebook_with_source("b"));
        assert_eq!(replace.path.as_deref(), Some("/tmp/replace.ipynb"));
        assert!(matches!(replace.kind, DeltaKind::Loaded { .. }));
    }
```

- [ ] **Step 2:** Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute notebook_store` → PASS (new + existing).

- [ ] **Step 3:** TS — add a `reconcileNotebookDelta` drop test to `notebook.test.ts`. Use a minimal stub notebook that records calls; import `reconcileNotebookDelta`:

```ts
import { reconcileNotebookDelta } from "./notebook";

function stubNotebook(path: string | undefined) {
  const calls = { applied: 0, resynced: 0 };
  const notebook = {
    state: {
      viewState: { path },
      serverState: { lastAppliedVersion: 1 },
    },
    applyNotebookDelta: () => {
      calls.applied += 1;
    },
    resyncFromSnapshot: async () => {
      calls.resynced += 1;
    },
  };
  // reconcileNotebookDelta only touches the members above.
  return { notebook: notebook as unknown as import("./notebook").Notebook, calls };
}

describe("reconcileNotebookDelta path guard", () => {
  it("drops a foreign-path delta without applying or resyncing", async () => {
    const { notebook, calls } = stubNotebook("/tmp/a.ipynb");
    await reconcileNotebookDelta(notebook, {
      version: 2,
      path: "/tmp/b.ipynb",
      kind: { type: "cellDeleted", id: "c1" },
    } as never);
    expect(calls.applied).toBe(0);
    expect(calls.resynced).toBe(0);
  });

  it("applies a matching-path delta", async () => {
    const { notebook, calls } = stubNotebook("/tmp/a.ipynb");
    await reconcileNotebookDelta(notebook, {
      version: 2,
      path: "/tmp/a.ipynb",
      kind: { type: "cellDeleted", id: "c1" },
    } as never);
    expect(calls.applied).toBe(1);
  });
});
```

- [ ] **Step 4:** Run: `scripts/spur-pnpm test -- src/stores/notebook.test.ts` → PASS (fall back to `SPUR_REMOTE=0` on the known infra error). Clean worktree; commit exactly once: `test(spur-notebook): cover Loaded-delta path stamping and reconcile path-drop`.

---

## Self-Review
- **Coverage:** Review findings #1 (handlers guard) + #5 (typed cast) → task-1; #3 (test gaps, Rust Loaded + TS reconcile-drop) → task-2. #2 (canonicalization), #4 (resync/single-store), agent-bridge broadcast → deferred to beads (stated above). ✅
- **Placeholders:** none; concrete code; helpers verified to exist. ✅
- **DAG:** task-1 ∥ task-2, file-isolated (task-1: notebook.ts + handlers.ts; task-2: notebook_store.rs + notebook.test.ts) → no collision. ✅
- **beads:** both tasks have IDs, empty depends_on, verifiable criteria, scope bounds with explicit "no scope_drift for minor decisions." ✅
