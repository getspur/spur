# Notebook Save / Store Hardening Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source:** Post-merge code review of the notebook-store-source-of-truth phase-2 work
(branch `main` @ `c2139e2a`). Two grounded review findings, neither a regression.

**Goal:** Harden the `notebook.save` store-routing path: (1) make the OPEN-vs-CLOSED
branch robust to path-spelling drift, and (2) keep the datasource catalog fresh when a
full-notebook replace changes datasource metadata.

**Architecture:** Build on the assembled phase-2 work already on `main`. `save.rs` routes
an open notebook's save through `NotebookStore::replace()`; the daemon `ReplaceNotebook`
command is the bridge mirror. Task 1 fixes the path comparison in `save.rs`. Task 2
centralizes "replace + hydrate datasource catalog + emit" in one reachable jute helper and
calls it from both the daemon arm and `save.rs`.

**Tech Stack:** Rust 2021, tokio, rmcp, jute notebook store. Build/test via
`scripts/spur-cargo` (remote-default). A red remote test is a real failure.

---

## Task 1: Robust OPEN-notebook detection in `notebook.save`

**Task ID:** `t1-save-path-robust`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/save.rs`
- Test: `crates/spur-notebook/src/mcp/tools/save.rs` (inline `#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A save whose path is a symlink/alias resolving to the same file as the store's
      open path routes through `NotebookStore::replace()` (store snapshot reflects it).
- [ ] New failing-first test `save_to_symlinked_open_path_routes_through_store` passes.
- [ ] Existing tests stay green: `save_to_open_notebook_routes_through_store`,
      `refuses_to_clobber_non_empty_with_empty_cells`, `force_flag_allows_empty_overwrite`,
      `empty_save_allowed_when_no_existing_file`, `saves_notebook_to_path`.
- [ ] `scripts/spur-cargo test -p spur-notebook` green; clippy clean.

**Suggested Worker:** codex (single file, mechanical)

**Scope Boundary:**
- IN scope: `crates/spur-notebook/src/mcp/tools/save.rs` only.
- OUT of scope: `SaveCoordinator`, `NotebookStore`, `commands.rs`, `loopback_requester.rs`.
- This task touches exactly ONE file. If you believe you need any other file, emit
  `scope_drift` — you should not.

**Implementation:**

- [ ] **Step 1: Write the failing test.** Add to the `tests` module in `save.rs`:

```rust
#[tokio::test]
async fn save_to_symlinked_open_path_routes_through_store() {
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-mcp-save-symlink-")
        .tempdir()
        .expect("temp dir");
    // Real file the store is opened against.
    let real = temp_dir.path().join("real.ipynb");
    tokio::fs::write(&real, serde_json::to_vec(&sample_notebook()).unwrap())
        .await
        .expect("seed disk");
    let state = Arc::new(State::new());
    state.get_notebook().load(&real, sample_notebook());

    // A symlinked alias spelling that resolves to the same file.
    let alias = temp_dir.path().join("alias.ipynb");
    std::os::unix::fs::symlink(&real, &alias).expect("symlink");

    let replacement: NotebookRoot = serde_json::from_value(json!({
        "metadata": {},
        "nbformat_minor": 5,
        "nbformat": 4,
        "cells": [
            { "cell_type": "markdown", "id": "cell-1", "metadata": {}, "source": "replacement" }
        ]
    }))
    .expect("replacement parses");
    let deps = deps_with_state(Arc::clone(&state));

    call(
        &deps,
        json!({ "path": alias.display().to_string(), "contents": replacement.clone() }),
    )
    .await
    .expect("save succeeds");

    // Routed through the store despite the alias spelling.
    let (snapshot, _version) = state.get_notebook().snapshot();
    assert_eq!(snapshot, replacement);
}
```

- [ ] **Step 2: Run it, watch it fail.** `scripts/spur-cargo test -p spur-notebook save_to_symlinked_open_path_routes_through_store`
      Expected FAIL: byte-for-byte `notebook.path() == path` is false for the alias, so the
      else-branch writes disk directly and the store snapshot still says `"saved"`.

- [ ] **Step 3: Make the comparison resolve-aware.** Replace the byte equality at the
      OPEN-vs-CLOSED branch with a small file-identity helper. Keep the fast path (exact
      equality) and fall back to canonicalization only when the raw paths differ and both
      canonicalize successfully (a not-yet-existing target falls through to the CLOSED path,
      preserving today's behavior):

```rust
use std::path::Path;

/// True when `candidate` denotes the same on-disk file the store currently holds open.
fn is_same_open_target(store_path: Option<&Path>, candidate: &Path) -> bool {
    let Some(store_path) = store_path else { return false };
    if store_path == candidate {
        return true;
    }
    matches!(
        (std::fs::canonicalize(store_path), std::fs::canonicalize(candidate)),
        (Ok(a), Ok(b)) if a == b
    )
}
```

  Then in `call`, replace:

```rust
    let notebook = state.get_notebook();
    if notebook.path().as_deref() == Some(path.as_path()) {
        notebook.replace(path, params.contents);
    } else {
```

  with:

```rust
    let notebook = state.get_notebook();
    if is_same_open_target(notebook.path().as_deref(), &path) {
        notebook.replace(path, params.contents);
    } else {
```

  Leave the empty-overwrite force guard, the `save_coordinator.save` else-branch, and the
  `notebook://saved` emit unchanged.

- [ ] **Step 4: Run it, watch it pass.** `scripts/spur-cargo test -p spur-notebook` — new test
      green, all existing save tests green.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-notebook/src/mcp/tools/save.rs
git commit -m "fix(spur-notebook): resolve-aware open-notebook detection in notebook.save"
```

---

## Task 2: Keep datasource catalog fresh on full-notebook replace

**Task ID:** `t2-replace-hydrate-catalog`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs`
  (the `DaemonControlCommand::ReplaceNotebook` arm in `handle_daemon_control_inner`; add a
  reusable hydrate-and-replace helper)
- Modify: `crates/spur-notebook/src/mcp/tools/save.rs` (open-notebook branch calls the helper)
- Test: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (inline tests)

**Depends on:** `t1-save-path-robust` (both edit `save.rs`; sequencing avoids a conflict —
your branch is based on Task 1's result and already contains its `save.rs` change).

**Acceptance Criteria:**
- [ ] Replacing an open notebook whose `metadata` carries different datasource setup
      refreshes `state.datasource_catalog` and emits the datasources-changed event — the
      same hydration `LoadNotebook` performs.
- [ ] New failing-first test in `commands.rs` proves the daemon `ReplaceNotebook` path
      refreshes the catalog.
- [ ] `save.rs` open-notebook branch routes through the shared helper so the LIVE save path
      gets the same hydration. All Task 1 `save.rs` tests stay green.
- [ ] `scripts/spur-cargo test -p spur-notebook -p jute` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope, all EXPECTED — do NOT emit `scope_drift` for editing these:
  - `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (helper + arm + test)
  - `crates/spur-notebook/src/mcp/tools/save.rs` (call the helper)
  - `crates/spur-notebook/jute-notebook/src-tauri/src/state.rs` ONLY if a `pub` accessor is
    needed to make the helper reachable from `spur-notebook`.
- This is a deliberate cross-crate (jute + spur-notebook) consistency fix; the multi-file,
  multi-crate footprint is intended.
- OUT of scope: changing `NotebookStore::replace()`'s signature, `SaveCoordinator`, the
  `loopback_requester.rs` mapping.

**Implementation:**

- [ ] **Step 1: Add a reusable helper in jute `commands.rs`.** Factor the hydrate+replace+emit
      sequence so both the daemon arm and `save.rs` share one code path (and it is `pub` /
      reachable from `spur-notebook`):

```rust
/// Replace the open notebook AND refresh the datasource catalog from its metadata,
/// mirroring what `LoadNotebook` does. Returns the store delta.
pub fn replace_notebook_and_hydrate_catalog(
    state: &State,
    path: PathBuf,
    contents: NotebookRoot,
) -> jute_notebook_store::NotebookDelta {
    let catalog = crate::state::DatasourceCatalog::hydrate_from_metadata(
        &contents.metadata,
        Some(path.as_path()),
    );
    let entries = catalog.list();
    *state.datasource_catalog.lock() = catalog;
    state.emit_datasources_changed(entries);
    state.get_notebook().replace(path, contents)
}
```

  (Use the crate's actual `NotebookDelta` path/import as it appears in this file — match the
  return type already used by the `ReplaceNotebook` arm.)

- [ ] **Step 2: Route the daemon arm through the helper.** In `handle_daemon_control_inner`:

```rust
        DaemonControlCommand::ReplaceNotebook { path, contents } => {
            let delta = replace_notebook_and_hydrate_catalog(state, PathBuf::from(path), contents);
            Ok(DaemonControlResult::Delta(delta))
        }
```

- [ ] **Step 3: Write the failing test (catalog refresh on replace).** In the `commands.rs`
      tests module, model it on the existing datasource/load tests — build a `State`, load a
      notebook whose metadata declares datasource A, call `ReplaceNotebook` (or the helper)
      with metadata declaring datasource B, then assert `state.datasource_catalog.lock().list()`
      reflects B and no longer A. (If catalog construction needs the
      `datasource-introspect` feature, gate the test the same way neighboring datasource
      tests are gated.) Run it and watch it fail before Step 1/2 are wired.

- [ ] **Step 4: Call the helper from `save.rs`.** In the OPEN-notebook branch (the
      `is_same_open_target` true arm from Task 1), replace the direct
      `notebook.replace(path, params.contents)` with the jute helper so the live save path
      hydrates too:

```rust
        jute::commands::replace_notebook_and_hydrate_catalog(state, path, params.contents);
```

  (Use the correct import path for the helper as exported by the jute crate. Keep the saved
  event emit and the force guard intact.)

- [ ] **Step 5: Run the suite.** `scripts/spur-cargo test -p spur-notebook -p jute` — new test
      green, all Task 1 `save.rs` tests green, all existing jute store/datasource tests green.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs \
        crates/spur-notebook/src/mcp/tools/save.rs
# include state.rs only if a pub accessor was added
git commit -m "fix(spur-notebook): hydrate datasource catalog on full-notebook replace"
```

---

## Self-Review

1. **Coverage:** Finding #1 → Task 1 (resolve-aware path comparison). Finding #2 → Task 2
   (catalog hydration on replace, both daemon arm and live save path). Both covered.
2. **Placeholders:** None — concrete test + impl code for every code step.
3. **Type consistency:** Task 2 reuses `DatasourceCatalog::hydrate_from_metadata` /
   `emit_datasources_changed` / `datasource_catalog` exactly as they appear in the existing
   `LoadNotebook` arm; `is_same_open_target` (Task 1) is consumed only within `save.rs`.
4. **DAG:** `t1 → t2`. Acyclic. Sequenced because both edit `save.rs` (a true file
   dependency, not artificial serialization).
5. **beads compatibility:** Each task has a unique ID, explicit `depends_on`, verifiable
   acceptance criteria, and a scope boundary that names every allowed file to pre-empt the
   scope-drift wedge seen in prior phases.
