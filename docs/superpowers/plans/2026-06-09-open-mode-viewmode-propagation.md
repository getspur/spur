# Auto-propagate `open_mode` → app `viewMode` Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan`. Each task is a beads issue under epic `bd-bs4f`.

**Source spec:** `docs/superpowers/specs/2026-06-09-open-mode-viewmode-propagation-design.md`
**Design epic:** `bd-bs4f` (open)

**Goal:** When an app-entry notebook (sibling `spur-app.json` with `open_mode=="app"`) is loaded
in the frontend, auto-enter app `viewMode`.

**Architecture:** A jute-local Tauri command `notebook_open_mode(path)` reads the sibling manifest
(no crate cycle — `commands.rs` is in crate `jute`, which `spur-notebook` depends on, so it cannot
import `SpurAppManifest`). The frontend load path calls it and sets `viewMode`.

**Tech Stack:** Rust (Tauri command, crate `jute`), TypeScript (zustand store, vitest).

---

## Task 1: `notebook_open_mode` Tauri command + registration

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (add command + local struct + test)
- Modify: `crates/spur-notebook/src/main.rs:379` (register in `tauri::generate_handler![ … ]`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `notebook_open_mode("<dir>/app.ipynb")` → `Some("app")` when sibling `spur-app.json` has
      `{"open_mode":"app","entry_notebook":"app.ipynb"}`.
- [ ] A different file in the same dir → `None`; no manifest → `None`; unparseable manifest → `None`.
- [ ] Command registered in `generate_handler!`.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute` passes.

**Scope Boundary:**
- IN: `commands.rs`, `main.rs` handler list.
- OUT: `SpurAppManifest` (do NOT import it — crate cycle), the daemon (`mcp/mod.rs`), frontend.
- If you need to touch out-of-scope files → emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: failing test** in the existing `#[cfg(test)] mod tests` (≈ commands.rs:2102;
  `tempfile` already used at :2236/:2481). Match the existing async test attribute (`#[tokio::test]`):

```rust
#[tokio::test]
async fn notebook_open_mode_app_entry_returns_app() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("spur-app.json"),
        r#"{"open_mode":"app","entry_notebook":"app.ipynb"}"#,
    )
    .expect("write manifest");
    let entry = dir.path().join("app.ipynb");
    std::fs::write(&entry, "{}").expect("write nb");
    assert_eq!(
        notebook_open_mode(entry.to_string_lossy().into_owned()).await.unwrap(),
        Some("app".to_string())
    );

    let other = dir.path().join("other.ipynb");
    std::fs::write(&other, "{}").expect("write nb2");
    assert_eq!(
        notebook_open_mode(other.to_string_lossy().into_owned()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn notebook_open_mode_no_manifest_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("app.ipynb");
    std::fs::write(&entry, "{}").expect("write nb");
    assert_eq!(
        notebook_open_mode(entry.to_string_lossy().into_owned()).await.unwrap(),
        None
    );
}
```

- [ ] **Step 2: run, verify fail** — `SPUR_REMOTE=1 scripts/spur-cargo test -p jute notebook_open_mode`
  → FAIL (function not defined).

- [ ] **Step 3: implement** in `commands.rs` (place near `get_notebook`, ≈ :1693). Mirror the daemon
  gate; manifest errors are non-fatal (`Ok(None)`):

```rust
#[derive(serde::Deserialize)]
struct AppModeManifest {
    open_mode: String,
    entry_notebook: String,
}

#[tauri::command]
/// Return the spur-app `open_mode` for a notebook when its sibling `spur-app.json`
/// names it as the `entry_notebook`. Missing or invalid manifest is non-fatal (`None`).
pub async fn notebook_open_mode(path: String) -> Result<Option<String>, Error> {
    let nb_path = std::path::Path::new(&path);
    let Some(dir) = nb_path.parent() else {
        return Ok(None);
    };
    let manifest_path = dir.join("spur-app.json");
    let content = match tokio::fs::read_to_string(&manifest_path).await {
        Ok(content) => content,
        Err(_) => return Ok(None),
    };
    let manifest: AppModeManifest = match serde_json::from_str(&content) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    if nb_path.file_name().and_then(|name| name.to_str()) == Some(manifest.entry_notebook.as_str())
    {
        Ok(Some(manifest.open_mode))
    } else {
        Ok(None)
    }
}
```

  If `Error` is not in scope at this location, use the same `Error` alias the sibling commands use
  (e.g. `get_notebook`, `move_notebook_to_trash`). `serde`, `serde_json`, and `tokio` are existing
  deps of the `jute` crate.

- [ ] **Step 4: register** in `crates/spur-notebook/src/main.rs` at the `tauri::generate_handler![`
  list (≈ :379). Add `notebook_open_mode,` adjacent to the existing `get_notebook` entry, matching
  its path style (bare or `commands::`-qualified — copy whatever `get_notebook` uses).

- [ ] **Step 5: run, verify pass** — `SPUR_REMOTE=1 scripts/spur-cargo test -p jute notebook_open_mode`
  → PASS. Then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings`.

- [ ] **Step 6: commit** — `feat(jute): bd-bs4f add notebook_open_mode tauri command`.

**Suggested Worker:** codex (single-crate, mechanical, well-specified).

---

## Task 2: frontend auto-enter app viewMode on notebook load

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:1404-1418` (`loadNotebookFromPath`)
- Test: `crates/spur-notebook/jute-notebook/src/stores/__tests__/notebook.test.ts`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] After load, when `notebook_open_mode` resolves to `"app"`, store `viewMode === "app"`.
- [ ] When it resolves to `null`/`"cells"` (or rejects), `viewMode` stays `"cells"`.
- [ ] `SPUR_REMOTE=1 scripts/spur-pnpm test -- src/stores/__tests__/notebook.test.ts` passes.

**Scope Boundary:**
- IN: `loadNotebookFromPath` + its test.
- OUT: the Tauri command (task-1), `NotebookHeader`, `AppMode`.
- If you need to touch out-of-scope files → emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: failing test** in `notebook.test.ts` — mock `invoke` (follow the file's existing
  `vi.mock("@tauri-apps/api/core", …)` / invoke-mock pattern). Make `get_notebook` return a minimal
  `NotebookRoot` and `notebook_open_mode` return `"app"`; assert
  `store.getState().viewState.viewMode === "app"` after `await loadNotebookFromPath("/x/app.ipynb")`.
  Add a second case where `notebook_open_mode` returns `null` ⇒ `viewMode === "cells"`.

- [ ] **Step 2: run, verify fail** —
  `SPUR_REMOTE=1 scripts/spur-pnpm test -- src/stores/__tests__/notebook.test.ts` → FAIL.

- [ ] **Step 3: implement** — extend `loadNotebookFromPath` (best-effort; never block load):

```ts
  /** Load a notebook from a file path. */
  async loadNotebookFromPath(path: string) {
    try {
      this.state.viewStateActions.startLoading();
    } catch {
      return;
    }
    try {
      const notebook = await invoke<NotebookRoot>("get_notebook", { path });
      this.loadNotebook(notebook);
      this.state.viewStateActions.setPath(path);
      try {
        const openMode = await invoke<string | null>("notebook_open_mode", { path });
        if (openMode === "app") {
          this.state.viewStateActions.setViewMode("app");
        }
      } catch {
        // best-effort: leave viewMode at its loaded default ("cells")
      }
    } catch (e: any) {
      this.state.viewStateActions.setLoadError(e.toString());
    }
  }
```

- [ ] **Step 4: run, verify pass** —
  `SPUR_REMOTE=1 scripts/spur-pnpm test -- src/stores/__tests__/notebook.test.ts` → PASS.
  Then `SPUR_REMOTE=1 scripts/spur-pnpm run typecheck`.

- [ ] **Step 5: commit** — `feat(jute-notebook): bd-bs4f auto-enter app viewMode on app-notebook open`.

**Suggested Worker:** codex (single-file frontend wiring + test).

---

## DAG

```
task-1 (Rust command + register)  →  task-2 (frontend load wiring)
```

## Self-Review

- **Spec coverage:** T1 = command+gate+registration (spec Approach bullets 1-2); T2 = frontend wiring
  (bullet 3). Both acceptance criteria map to the epic acceptance. ✓
- **Placeholders:** none — full code for command, tests, and frontend edit. ✓
- **Type consistency:** `notebook_open_mode(path: String) -> Result<Option<String>, Error>` is the
  same contract the frontend invokes as `invoke<string | null>`. ✓
- **DAG:** linear, acyclic, T2 depends T1. ✓
- **beads:** both tasks tracked under epic `bd-bs4f` with unique IDs, scope boundaries, and
  verifiable acceptance. ✓
