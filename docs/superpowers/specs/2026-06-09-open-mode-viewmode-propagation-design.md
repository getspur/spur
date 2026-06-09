# Auto-propagate spur-app `open_mode` → frontend app `viewMode` on notebook open

- **Date:** 2026-06-09
- **Status:** Approved
- **Owner:** brain
- **Worker:** codex
- **Design epic:** `bd-bs4f`
- **Design notebook:** `docs/superpowers/designs/2026-06-09-spur-notebook-app-architecture.ipynb` (committed `5d66d839a`)

## Problem (verified against the worktree graph)

The open-design review mapped the App↔Foundation boundary and confirmed a load-bearing
disconnect (design Panel C/E #1):

1. **`open_mode` is Rust-daemon-only.** It exists solely as the `SpurAppManifest::open_mode`
   field (`crates/spur-notebook/src/spur_app.rs:31`) and is consumed only by the daemon's
   bootstrap gate `app_plugin_config_for_notebook` (`crates/spur-notebook/src/mcp/mod.rs:3000-3036`):
   the app's MCP server spawns iff `open_mode=="app"` **AND** the opened file's name ==
   `entry_notebook` **AND** `mcp_server` is present.
2. **`viewMode` is an independent frontend concept.** `NotebookViewMode = "cells"|"dag"|"app"`
   (`crates/spur-notebook/jute-notebook/src/stores/notebook.ts:111`), initialized to `"cells"`
   and changed only by the user via the `NotebookHeader` segmented control.
3. **Structural proof of the gap:** there is **zero** `open_mode`/`openMode` reference anywhere
   in the `.ts/.tsx` tree. The graph indexes the frontend (it returns `setViewMode`, `VIEW_MODES`,
   etc.), so the absence is real, not an indexing blind spot.

Consequently, opening an app notebook spawns its plugin (tools become callable) but the UI does
**not** auto-enter app mode. The user must manually flip the segmented control — the
open→bootstrap→functional journey is incomplete on the UX side.

## Decisive architectural constraint

The crate dependency direction is **`spur-notebook → jute`**
(`crates/spur-notebook/Cargo.toml`: `jute = { path = "jute-notebook/src-tauri" }`). The Tauri
frontend bridge `commands.rs` lives in crate **`jute`**; `SpurAppManifest` lives in crate
**`spur-notebook`**. Therefore `commands.rs` **cannot** import `SpurAppManifest` without creating
a dependency cycle. The fix must read the sibling `spur-app.json` **locally in `jute`** via a
minimal struct.

## Approach (chosen)

Add a jute-local Tauri command that mirrors the daemon's gate on the frontend load path:

- **`notebook_open_mode(path) -> Option<String>`** in `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs`:
  read the sibling `spur-app.json` (filename `"spur-app.json"`), deserialize a minimal local
  struct `{ open_mode: String, entry_notebook: String }` (ignore the rest with serde), and return
  `Some(open_mode)` **iff** `Path::new(&path).file_name()` equals `entry_notebook`. Missing file,
  read error, or parse error → `Ok(None)` (non-fatal; never blocks notebook load).
- Register the command in the Tauri handler list at `crates/spur-notebook/src/main.rs:379`
  (`tauri::generate_handler![ … ]`).
- **Frontend wiring** in `loadNotebookFromPath` (`…/src/stores/notebook.ts:1404-1418`): after
  `this.loadNotebook(notebook)` + `setPath(path)`, call
  `invoke<string|null>("notebook_open_mode", { path })`; if it resolves to `"app"`, call
  `this.state.viewStateActions.setViewMode("app")`. The call is best-effort (a rejected/`null`
  result leaves `viewMode` at its loaded default of `"cells"`).

### Rejected alternatives

- **Reuse `SpurAppManifest` in `commands.rs`** — impossible without a `jute → spur-notebook → jute`
  cycle.
- **Thread `open_mode` through the daemon-control snapshot/loaded payload** — architecturally
  "single source of manifest truth" but invasive (new field across the daemon response, the
  Tauri bridge, and the frontend delta path). Deferred; the bounded jute-local read is the lower-risk
  v1 and the 2-field duplication is trivial and stable.

## Decomposition (DAG)

### T1 — `notebook_open_mode` Tauri command + registration *(no deps)*
- Add the command + a minimal local manifest struct in `commands.rs`; register it in
  `spur-notebook/src/main.rs` `generate_handler!`.
- **TDD:** unit test in the existing `commands.rs` `#[cfg(test)] mod tests` (≈ line 2102; `tempfile`
  already in scope) — temp dir with `spur-app.json` (`open_mode:"app"`, `entry_notebook:"app.ipynb"`):
  `app.ipynb` path → `Some("app")`; a different file in the same dir → `None`; a dir with no
  manifest → `None`; `open_mode:"notebook"` entry file → `Some("notebook")` (frontend ignores
  non-"app").
- **Acceptance:** `scripts/spur-cargo test -p jute` green; command registered.

### T2 — frontend auto-enter app viewMode on load *(depends: T1)*
- Wire `loadNotebookFromPath` to call `notebook_open_mode` and `setViewMode("app")` on `"app"`.
- **TDD:** vitest in `…/src/stores/__tests__/notebook.test.ts` (mock `invoke` per the existing
  pattern): `notebook_open_mode → "app"` ⇒ store `viewMode === "app"` after load; `→ null`/`"cells"`
  ⇒ stays `"cells"`.
- **Acceptance:** `scripts/spur-pnpm test -- src/stores/__tests__/notebook.test.ts` green.

## Constraints (all tasks)

- Build/test only through `scripts/spur-cargo` and `scripts/spur-pnpm` (never bare cargo/pnpm).
- No crate cycle; manifest read stays in `jute`.
- Gate mirrors the daemon: `file_name == entry_notebook`; manifest errors → `None`.
- Strictly scoped to `open_mode → viewMode`. App shell/identity, bootstrap-status HUD, and
  app-asset packaging (design Panel E #2-#4) are OUT of scope.

## Acceptance (epic)

Opening a notebook whose sibling `spur-app.json` declares `open_mode=="app"` for that entry
auto-enters app `viewMode`; all other notebooks stay in `"cells"`. Covered by a Rust unit test
(T1) and a frontend test (T2); brain does a best-effort live check in Jute.
