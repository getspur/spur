# Notebook-Rooted Spur Apps — Complete Embedded-Manifest Coverage (Design Spec)

- **Date:** 2026-06-15
- **Design epic:** `bd-o38b9`
- **Plan id:** `wc-app-notebook-root-2026-06-15`
- **Status:** Approved (Approach 1)
- **Driver:** world-cup-2026 app should "import the source file as root" — the notebook file alone is a sufficient app root, with no dependency on a sibling `spur-app.json` or an ambient working directory.

## Problem

The notebook-as-app platform gained a *self-describing* manifest embedded in
`notebook.metadata.spur_app` (merged to `main` as `99223fcfd`). But that feature
only covered **4 of the 6** places that load an app manifest. Each consumer
re-implements manifest loading inline, so the two paths that actually **launch
App mode** were missed and still read the sibling `spur-app.json` from disk:

| Consumer | File | Embedded-aware? | Effect if `spur-app.json` is absent |
|---|---|---|---|
| `resolve_app_scope` (sidebar agent) | `crates/spur-notebook/src/sidebar_chat/scope.rs:14` | ✅ merged | fine |
| `app_section` (context pack) | `crates/spur-notebook/src/context/pack.rs:72` | ✅ merged | fine |
| `notebook_app_doctor` | `crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs` | ✅ merged | fine |
| `export_spur_app` | `crates/spur-notebook/src/spur_app.rs:259` | ✅ merged | fine |
| **`notebook_open_mode`** (decides App vs normal mode + capabilities) | `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1923` | ❌ reads `dir/spur-app.json` | **notebook never enters App mode** — no `active_output_scripts`, no app chrome |
| **`app_plugin_config_for_notebook`** (spawns the app's MCP plugin, e.g. `wc_*`) | `crates/spur-notebook/src/mcp/mod.rs:3583` | ❌ reads `app_root/spur-app.json` | **app MCP tools never spawn** |

Both launch paths already anchor the child process at the notebook's parent
directory — `build_command` sets `.current_dir(&config.working_dir)`
(`crates/spur-notebook/src/mcp/plugin_loader.rs:381`) and `entry`/`requirements`
are relative args resolved against it. The remaining coupling is twofold:

1. They require `spur-app.json` to physically sit next to the notebook (so the
   notebook cannot be the sole source of truth).
2. `app_root` is derived from `notebook_path.parent()` without canonicalization;
   when the notebook is opened via a **relative** path, `working_dir` is relative
   and `current_dir` resolves against the **daemon's process cwd** — the "needs a
   cwd" failure.

## Goal

Make the notebook file a sufficient App root. After this change, a world-cup
notebook carrying an embedded `metadata.spur_app` manifest opens in App mode and
spawns its MCP plugin **with no `spur-app.json` on disk** and **regardless of the
daemon's process cwd**. Sidecar source files (`server/*.py`, `sdk/`, `skill/`)
remain on disk (decision **B** — "still multi-file"); only the *manifest* moves
into the notebook.

Non-goal: embedding the Python/SDK source into the notebook (that was option A,
rejected). Non-goal: any change to the world-cup app contents in this work item
(that is a sequenced brain follow-up — see "Out of scope / sequencing").

## Approach (Approach 1 — shared resolver)

Root-cause fix: replace the N inline manifest loaders with **one resolver per
crate** and route every consumer through it. This is what prevents the next
consumer from drifting.

### Core resolver

Add to `crates/spur-notebook/src/spur_app.rs`:

```rust
pub enum ManifestSource { Embedded, SiblingJson(PathBuf) }

/// Embedded-first, sibling-`spur-app.json` fallback, absolute app_root.
/// Returns None when neither source is present or the manifest is invalid.
pub fn resolve_app_manifest(
    notebook_path: &Path,
) -> Option<(PathBuf /* absolute app_root */, SpurAppManifest, ManifestSource)>;
```

Semantics:
- **Absolutize first.** Resolve `notebook_path` to an absolute path
  (`std::fs::canonicalize`, falling back to `current_dir().join(path)` if the
  file cannot be canonicalized) before deriving `app_root = parent`. This is the
  P3 "no cwd" guarantee — `app_root` (and therefore every spawned
  `working_dir`) is always absolute.
- **Embedded first.** Reuse the existing embedded reader logic
  (`manifest_from_notebook`, `crates/spur-notebook/src/spur_app.rs:216`): parse
  `root.metadata.other["spur_app"]` → `SpurAppManifest`; default
  `entry_notebook` to the notebook's file name when empty.
- **Sibling fallback.** If no embedded manifest, read
  `app_root/spur-app.json` and parse `SpurAppManifest` (mirror the existing
  `find_manifest_dir` / sibling read in `resolve_app_scope`).
- Keep `manifest_from_notebook` as a thin wrapper (or inline it into the
  resolver) so existing callers/tests do not break.

Route through `resolve_app_manifest`:
- `resolve_app_scope` (`scope.rs:14`) — keep `AppScope.cwd = app_root` (now
  absolute); behavior otherwise unchanged.
- `app_section` (`context/pack.rs:72`).
- `notebook_app_doctor`.
- `export_spur_app` (`spur_app.rs:259`).
- **`app_plugin_config_for_notebook` (`mcp/mod.rs:3583`) — P2.** Replace the
  `app_root.join(SPUR_APP_MANIFEST)` read with `resolve_app_manifest`. Preserve
  the existing gates (`manifest.open_mode == "app"`, notebook file name ==
  `manifest.entry_notebook`). Build `PluginConfig::from_manifest(name, server,
  app_root)` with the **absolute** `app_root` as `working_dir`. Host-provisioned
  env + `artifacts_dir` side-effects unchanged.

### src-tauri consumer (P1)

`notebook_open_mode` (`commands.rs:1923`) lives in the jute `src-tauri` crate,
which already depends on `spur_notebook`. Rewrite it to call the **core**
`spur_notebook::spur_app::resolve_app_manifest`, then map `SpurAppManifest` →
`NotebookOpenInfo`:
- `open_mode` ← `manifest.open_mode` (return `None` unless `"app"`).
- gate: notebook file name == `manifest.entry_notebook`, else `None`.
- `app_name` ← `manifest.name` (default `"App"`).
- `app_root` ← absolute app_root from the resolver (string).
- `capabilities` ← map `SpurAppManifest.capabilities` →
  `AppModeCapabilities` (`ports`, `canvas_capture`, `active_output_scripts`,
  `artifacts_dir`).
- `skill` ← `manifest.skill` (default `"skill/SKILL.md"`).

Consolidation: the duplicated `src-tauri`
`spur_app.rs::manifest_from_notebook` (`jute-notebook/src-tauri/src/spur_app.rs:51`)
and the local `AppModeManifest` struct become redundant. Confirm callers with
`code_callers`; delete whichever has no remaining references after the rewrite.
If a small mapping struct is still needed for capabilities, keep only that.

## Acceptance Criteria

Platform (this delegation):
1. With **only** an embedded `metadata.spur_app` manifest and **no**
   `spur-app.json` on disk:
   - `notebook_open_mode` returns `Some` with `open_mode == "app"`, correct
     `app_name`, **absolute** `app_root`, and mapped capabilities.
   - `app_plugin_config_for_notebook` returns `Some(PluginConfig)` with an
     **absolute** `working_dir` and the manifest's `entry`/`requirements`.
2. Opening the entry notebook via a **relative** path still yields an absolute
   `app_root`/`working_dir` (no dependence on process cwd).
3. **Back-compat:** a sibling-`spur-app.json`-only app (no embedded manifest)
   still opens in App mode and spawns its plugin exactly as before.
4. Non-entry notebooks in the same dir still return `None` from
   `notebook_open_mode` (entry-notebook gate preserved).
5. All consumers (`resolve_app_scope`, `app_section`, doctor, export, P1, P2)
   call the single `resolve_app_manifest`; no remaining inline sibling-only reads
   for App launch.
6. New/updated tests cover: embedded-first precedence, sibling fallback,
   absolute-root-from-relative-path, and the back-compat path — modeled on the
   existing `embedded_manifest_takes_precedence_over_sibling_spur_app_json` and
   `notebook_open_mode_entry_returns_app` tests.
7. Green via remote build: `scripts/spur-cargo test -p spur-notebook` and the
   jute `src-tauri` crate tests; `SPUR_REMOTE=1 scripts/spur-cargo clippy
   --workspace -- -D warnings`. **Never bare cargo.**

## Out of scope / sequencing (brain follow-up, NOT this delegation)

The world-cup app pilot (A1 embed manifest + delete `spur-app.json`, A2 prune
the datasource-setup / `github_security_advisories()` / `linear_projects()`
cruft cells, A3 re-run `notebook_app_doctor`) is **deliberately excluded** from
the worker task. Order matters:

1. Land this platform change on `main`.
2. Rebuild/restart the notebook daemon so the running binary honors embedded
   manifests in the launch paths.
3. **Only then** the brain runs the app pilot through the `notebook_*` MCP tools.

Deleting `spur-app.json` before step 2 would break the live app on the old
daemon. Editing the world-cup app is therefore a separate, sequenced step the
brain performs via notebook tools — not a file edit in this worker branch.

## Risk notes

- Capabilities type mapping (`SpurAppManifest.capabilities` → src-tauri
  `AppModeCapabilities`) must be exact; a wrong `active_output_scripts` mapping
  would silently disable Perspective's scripts-on rendering.
- `canonicalize` fails if the path does not exist; on the open path the notebook
  always exists, but the resolver must still degrade gracefully (fallback to
  `current_dir().join`) rather than panic, so unit tests over tempdirs behave.
- Keep `manifest_from_notebook`'s `tracing::warn!` diagnostics on parse failure —
  silent `None` on a malformed embedded manifest is a debugging trap.
