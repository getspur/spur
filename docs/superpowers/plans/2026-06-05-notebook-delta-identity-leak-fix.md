# Notebook Delta Identity — Cross-Notebook Leak Fix Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** investigation in conversation (code-explore + spur-analyst, double-confirmed against fresh graph artifact `graph_content_hash 22571ce2…`)
**Design epic:** n/a (direct bugfix from confirmed root-cause analysis)

**Goal:** Stop a notebook mutation from leaking into every other open notebook window by stamping each `NotebookDelta` with its owning notebook path and dropping foreign-path deltas on the frontend.

**Architecture:** The daemon holds a single process-wide `NotebookStore`; it publishes path-less `NotebookDelta`s that `lib.rs` broadcasts globally via `app.emit("notebook://changed", …)`. Every notebook window registers a global listener and `reconcileNotebookDelta` applies each delta to its own notebook with only a version-gap guard — no notebook attribution. We add a `path` field to `NotebookDelta` (stamped from `NotebookStore::path()` at publish time) and an early path-match guard in `reconcileNotebookDelta` so a window only applies deltas for the notebook it is displaying. This is the containment fix; full multi-notebook support (keying the store by path) is an explicit out-of-scope follow-up.

**Tech Stack:** Rust (`jute` crate, `ts-rs`), TypeScript (React/Zustand frontend, Vitest).

---

## File Structure Mapping

- `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` — `NotebookDelta` struct + the 5 delta construction sites + a `make_delta` helper. (Task 1)
- `crates/spur-notebook/jute-notebook/src/bindings/` — regenerated ts-rs output (generated artifact, not hand-edited). (Task 1)
- `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` — `reconcileNotebookDelta` early guard + pure `notebookDeltaIsForPath` helper. (Task 2)
- `crates/spur-notebook/jute-notebook/src/stores/notebook.test.ts` — new Vitest unit test for the helper. (Task 2)

---

## Task 1: Stamp every NotebookDelta with its owning notebook path

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` (struct `NotebookDelta` ~144-150; sites at ~305, ~326, ~520, ~543, ~585; add `make_delta` helper)
- Regenerate: `crates/spur-notebook/jute-notebook/src/bindings/` (via `ts-rs-export` binary — do not hand-edit)
- Test: `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` (`#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `NotebookDelta` has a `path: Option<String>` field, defaulted/optional for serde + ts-rs backward compatibility.
- [ ] Every published delta (`load`, `replace`, `apply`, `apply_run_event`, `publish_dag_status_changed`) carries `path == self.path()` rendered as a string, or `None` when the store has no loaded path.
- [ ] New unit test passes; existing `notebook_store.rs` tests still pass.
- [ ] `scripts/spur-cargo run -p jute --bin ts-rs-export` regenerates `src/bindings/NotebookDelta.ts` with the new optional `path` field, committed alongside.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings` is clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `notebook_store.rs` `NotebookDelta` + delta construction + helper; regenerating `src/bindings/`.
- OUT of scope: keying the store by path / `DashMap` refactor; `state.rs`; `lib.rs` forwarder; the Tauri `agent://request` bridge; `reconcileNotebookDelta` (that is Task 2).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `notebook_store.rs` (helpers `store_with_notebook()` loading `/tmp/test.ipynb`, `CELL_ID`, and `NotebookStore::new` already exist):

```rust
    #[test]
    fn deltas_carry_owning_notebook_path() {
        let store = store_with_notebook(); // loads "/tmp/test.ipynb"

        let write = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "x = 1".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();
        assert_eq!(write.path.as_deref(), Some("/tmp/test.ipynb"));

        let run = store
            .apply_run_event(CELL_ID, RunCellEvent::Stdout("hi".to_string()))
            .unwrap();
        assert_eq!(run.path.as_deref(), Some("/tmp/test.ipynb"));
    }

    #[test]
    fn delta_path_is_none_before_any_notebook_is_loaded() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        let delta = store.publish_dag_status_changed(serde_json::json!({"nodes": []}));
        assert_eq!(delta.path, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute deltas_carry_owning_notebook_path delta_path_is_none_before_any_notebook_is_loaded`
Expected: FAIL — `NotebookDelta` has no field named `path`.

- [ ] **Step 3: Add the field**

In `NotebookDelta` (the struct at ~144). Keep the existing `version`/`kind` fields and derives; insert `path`:

```rust
pub struct NotebookDelta {
    /// Monotonic document version after the mutation.
    #[ts(type = "number")]
    pub version: u64,
    /// Worktree path of the notebook this delta belongs to. `None` only when the
    /// store has no loaded path yet (a fresh/unsaved store). Used by the frontend
    /// to drop deltas that belong to a different open notebook window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// Kind of mutation represented by this delta.
    pub kind: DeltaKind,
}
```

- [ ] **Step 4: Add a `make_delta` helper and route all construction sites through it**

Add this private method inside `impl NotebookStore` (near `path()` ~345):

```rust
    /// Build a delta stamped with the store's current notebook path.
    fn make_delta(&self, version: u64, kind: DeltaKind) -> NotebookDelta {
        NotebookDelta {
            version,
            path: self.path().map(|p| p.display().to_string()),
            kind,
        }
    }
```

Then replace each `NotebookDelta { version, kind: … }` / `NotebookDelta { version: …, kind: … }` literal with `self.make_delta(version, kind)`:

- `load` (~305): after the locked block sets the path, `let delta = self.make_delta(version, DeltaKind::Loaded { root: root_snapshot });`
- `replace` (~326): `let delta = self.make_delta(version, DeltaKind::Loaded { root: root_snapshot });`
- `apply` (~520): `let delta = self.make_delta(version, kind);`
- `apply_run_event` (~543): `let delta = self.make_delta(self.bump_version(), DeltaKind::RunCellEvent { cell_id, event });`
- `publish_dag_status_changed` (~585): `let delta = self.make_delta(self.version.load(Ordering::SeqCst), DeltaKind::DagStatusChanged { snapshot });`

`make_delta` re-locks `self.path` (via `self.path()`); in `load`/`replace` the path-mutex guard is already dropped before the delta is built, so there is no double-lock.

- [ ] **Step 5: Run test to verify it passes**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute deltas_carry_owning_notebook_path delta_path_is_none_before_any_notebook_is_loaded`
Expected: PASS. Then run the whole module: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute notebook_store`
Expected: PASS (existing tests unaffected — `path` is additive).

- [ ] **Step 6: Regenerate TypeScript bindings**

Run: `scripts/spur-cargo run -p jute --bin ts-rs-export`
Expected: `src/bindings/NotebookDelta.ts` now declares an optional `path?: string`. Confirm with `git diff -- crates/spur-notebook/jute-notebook/src/bindings/NotebookDelta.ts`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs \
        crates/spur-notebook/jute-notebook/src/bindings/
git commit -m "feat(spur-notebook): stamp NotebookDelta with owning notebook path"
```

**Scope Drift Checkpoint:**
- If clippy/test forces edits outside `notebook_store.rs` + `src/bindings/` → emit `scope_drift`.
- If `ts-rs-export` fails to run in this environment → emit `risk` (the brain will decide how to regenerate bindings).

---

## Task 2: Drop foreign-path deltas in reconcileNotebookDelta

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` (`reconcileNotebookDelta` ~981; add exported `notebookDeltaIsForPath` helper)
- Create: `crates/spur-notebook/jute-notebook/src/stores/notebook.test.ts`

**Depends on:** task-1 (the `NotebookDelta` binding must expose `path`)

**Acceptance Criteria:**
- [ ] `reconcileNotebookDelta` returns early (applies nothing, triggers no resync) when the delta's `path` is present, the notebook's `viewState.path` is present, and they differ after normalization.
- [ ] When either path is absent, the delta is still applied (backward compatibility for unsaved/scratch notebooks and pre-`path` builds).
- [ ] The early guard runs BEFORE the `hasAuthoritativeVersionGap` check, so a foreign-path delta can never trigger `resyncFromSnapshot`.
- [ ] New Vitest unit test passes; `scripts/spur-pnpm run typecheck` is clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `notebook.ts` `reconcileNotebookDelta` + the new pure helper; the new test file.
- OUT of scope: `events.ts`, `bridge.ts`, `lib.rs`, the `agent://request` path, store-keying refactor.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test**

Create `crates/spur-notebook/jute-notebook/src/stores/notebook.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import { notebookDeltaIsForPath } from "./notebook";

describe("notebookDeltaIsForPath", () => {
  it("applies a delta whose path matches the open notebook", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", "/tmp/a.ipynb")).toBe(true);
  });

  it("drops a delta whose path belongs to a different notebook", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", "/tmp/b.ipynb")).toBe(false);
  });

  it("ignores a trailing slash difference", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb/", "/tmp/a.ipynb")).toBe(true);
  });

  it("applies when the delta has no path (scratch / pre-path builds)", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", null)).toBe(true);
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", undefined)).toBe(true);
  });

  it("applies when the notebook has no path yet", () => {
    expect(notebookDeltaIsForPath(undefined, "/tmp/a.ipynb")).toBe(true);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `scripts/spur-pnpm test -- src/stores/notebook.test.ts`
Expected: FAIL — `notebookDeltaIsForPath` is not exported.

- [ ] **Step 3: Add the pure helper**

Add to `notebook.ts` (top-level, exported so the test can import it):

```ts
function normalizeNotebookPath(path: string): string {
  return path.replace(/\/+$/, "");
}

/**
 * Whether an authoritative delta belongs to the notebook displayed by this
 * window. The daemon holds a single process-wide store but broadcasts every
 * delta to all windows; this guard prevents a mutation to one notebook from
 * leaking into the others. When either side has no path (unsaved/scratch
 * notebook, or a pre-`path` daemon build) we apply the delta for backward
 * compatibility.
 */
export function notebookDeltaIsForPath(
  notebookPath: string | undefined,
  deltaPath: string | null | undefined,
): boolean {
  if (!notebookPath || !deltaPath) {
    return true;
  }
  return normalizeNotebookPath(notebookPath) === normalizeNotebookPath(deltaPath);
}
```

- [ ] **Step 4: Wire the guard into `reconcileNotebookDelta`**

`reconcileNotebookDelta` (~981) currently reads `lastAppliedVersion`, then checks `hasAuthoritativeVersionGap`, then `applyNotebookDelta`. Insert the guard as the FIRST statement so foreign deltas never reach the version-gap/resync path:

```ts
export async function reconcileNotebookDelta(
  notebook: Notebook,
  delta: AuthoritativeNotebookDelta,
) {
  if (
    !notebookDeltaIsForPath(
      notebook.state.viewState.path,
      (delta as { path?: string | null }).path,
    )
  ) {
    // Delta belongs to a different open notebook window; ignore it.
    return;
  }

  const lastAppliedVersion = notebook.state.serverState.lastAppliedVersion;
  if (hasAuthoritativeVersionGap(notebook.state.serverState, delta)) {
    // …unchanged…
    await notebook.resyncFromSnapshot();
    return;
  }

  notebook.applyNotebookDelta(delta);
}
```

The `(delta as { path?: string | null }).path` cast keeps the access valid across the `NotebookDelta | DagStatusChangedDelta` union; the regenerated `NotebookDelta` binding from Task 1 supplies the field at runtime.

- [ ] **Step 5: Run test + typecheck to verify it passes**

Run: `scripts/spur-pnpm test -- src/stores/notebook.test.ts`
Expected: PASS.
Run: `scripts/spur-pnpm run typecheck`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/stores/notebook.ts \
        crates/spur-notebook/jute-notebook/src/stores/notebook.test.ts
git commit -m "fix(spur-notebook): drop foreign-path notebook deltas to stop cross-notebook leak"
```

**Scope Drift Checkpoint:**
- If `delta.path` from real daemon traffic does not match `viewState.path` because of symlink/relative-vs-absolute normalization → emit `risk`. (Backend emits `NotebookStore::path()`, set on open via the daemon's `normalize_path`; if the window's `viewState.path` is a non-canonical URL string they may differ. The brain will decide whether to canonicalize on both sides.)
- If wiring the guard requires touching `events.ts`/`bridge.ts` → emit `scope_drift`.

---

## Out of Scope (follow-up epics)

1. **Multi-notebook authoritative store.** Key `state.notebook` by path (`DashMap<PathBuf, Arc<NotebookStore>>`, mirroring `kernels`) and thread `path` through the ~50 `get_notebook` call sites + mutate commands so two notebooks can be genuinely live at once. This plan only stops the leak; it does not make the single-store model multi-notebook.
2. **Tauri `agent://request` broadcast.** `bridge.rs:255` `app.emit("agent://request", …)` + `bridge.ts:33` global listener would leak the same way if a `TauriBridgeRequester`-with-app were ever wired for the brain (today the brain uses `LoopbackDaemonRequester`, so this path is dormant). Switch to `emit_to(window_label, …)` or carry a path when that path is activated.

---

## Self-Review

- **Spec coverage:** Root cause has three legs — (a) single store, (b) path-less delta, (c) blind global reconcile. This plan fixes (b) by adding `path` and (c) by filtering on it; (a) is explicitly deferred. The user-reported symptom (edits to one notebook leak into other open notebooks, on screen and disk) is resolved because foreign-path deltas are dropped before apply and before the autosave-triggering store mutation in the wrong window. ✅
- **Placeholder scan:** No TBD/TODO; all code blocks concrete; test helpers (`store_with_notebook`, `CELL_ID`, `NotebookStore::new`, `SaveCoordinator::default`) verified to exist. ✅
- **Type consistency:** `path: Option<String>` (Rust) ↔ `path?: string` (binding) ↔ `deltaPath: string | null | undefined` (helper). `make_delta(version: u64, kind: DeltaKind)` signature used consistently at all five sites. ✅
- **DAG validation:** task-1 → task-2 (linear; task-2 needs the binding). No cycles. Chain is minimal (2 tasks); they cannot be parallel because task-2 depends on task-1's binding. ✅
- **beads compatibility:** Both tasks have unique IDs, explicit `depends_on`, brain-verifiable acceptance criteria, and scope boundaries. ✅
