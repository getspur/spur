# Remote-build `spur-notebook` / `jute-notebook` — Design

- **Date:** 2026-05-30
- **Status:** Approved design, pre-implementation
- **Scope:** dev/internal install loop (build Linux binaries on the GCP VM, fetch
  back). macOS `.app` bundles stay local. Product-release artifacts stay on
  `cargo-dist` / `.github/workflows/release.yml`.
- **Validation:** Findings and direction double-checked against industry patterns
  by the `codex` worker (delegation `52faede1`, 2026-05-30); all four findings
  confirmed, direction assessed as industry-standard with four refinements (folded
  into this spec).

## 1. Background & problem

`cargo xtask install` (alias `cargo run --package xtask -- install`, `.cargo/config.toml`)
installs two binaries: `spur` (`crates/spur-cli`) and `spur-notebook`
(`crates/spur-notebook`). `spur-notebook` is a Tauri 2 desktop app ("Jute") whose
frontend is a Vite/React app in `crates/spur-notebook/jute-notebook`.

A GCP remote-build pipeline (`scripts/gcp-build/`) offloads compile-heavy cargo
subcommands to a debian-12 VM with GCS-backed sccache. `scripts/spur-cargo` routes
`build|check|test|clippy|doc` to the VM; everything else (including `xtask`) runs
locally.

### 1.1 Findings (all CONFIRMED by codex)

1. **Double dep-tree compile.** `install` (`xtask/src/main.rs:39`) calls
   `cargo_install` at two sites — `:43` (`crates/spur-cli`) and `:66`
   (`crates/spur-notebook`). `cargo_install_command` (`:94`) runs
   `cargo install --path <p> --force` with **no shared `--target-dir`**, so each
   pass builds in its own temp target dir. Everything the two binaries share
   (`spur-core`, tokio, datafusion, lance, the tauri tree, …) compiles from
   scratch **twice**, in release.

2. **sccache disabled on every platform.** `cargo_install_command:113` calls
   `.env_remove("RUSTC_WRAPPER")` **unconditionally**. The comment attributes it to
   a macOS `com.apple.provenance` vs sccache write collision, but the strip applies
   on **all** platforms and **both** passes — so the double compile above is also
   fully **uncached**, including on the Linux VM where that provenance bug does not
   exist. codex: unconditional strip is "too broad" — make it macOS-only.

3. **macOS triple-compiles + double-builds the frontend.** On macOS, `install`
   skips the `:66` install and instead runs `install_macos_jute_app` →
   `ensure_jute_frontend_dist` (`:130`, runs `npm run build`) then
   `tauri_build_command` (`:140`, `tauri build`). `tauri build` compiles
   `spur-notebook` again **and** re-runs `beforeBuildCommand: "npm run build"`
   (`crates/spur-notebook/tauri.conf.json`), rebuilding the frontend a second time.

4. **Remote pipeline cannot produce a runnable notebook.** The VM has no
   node/npm/pnpm (verified: all MISSING). `jute-notebook/dist` and `node_modules`
   are gitignored, so `build.sh`'s `git ls-files` sync never copies them; `build.sh`
   has no frontend build step. A plain `cargo check --workspace` *succeeds* remotely
   only because the `custom-protocol` feature is **off** → Tauri runs in dev mode
   (loads `devUrl`, does not embed `frontendDist`) → `generate_context!` does not
   need `dist`. The moment a build sets `custom-protocol` (production), it needs a
   real `dist` that the VM can neither sync nor build.

### 1.2 Why this matters

The naïve "run `xtask install` on the VM" would inherit findings 1–3 — two cold
uncached release builds remotely. The real win is restructuring so the notebook
stack builds **once, in the shared `target/`, through sccache**, with the frontend
built once beforehand.

## 2. Goals / non-goals

**Goals**
- Fix the `xtask install` double/triple compile and the over-broad sccache strip
  (valuable locally, independent of remote).
- Add a remote install path: build the Linux `spur` + `spur-notebook` on the VM
  in a single sccache-backed workspace release build, fetch both binaries back to
  `$CARGO_HOME/bin`.

**Non-goals**
- Remote macOS `.app` builds (cannot build/codesign a Mac bundle on Linux).
- Product-release artifacts (remain on `cargo-dist` / `release.yml`; releases that
  ship widely should use per-OS CI runners per Tauri norms).
- The npm→pnpm migration itself (kept npm; see §4 D2).
- Wide distribution of the fetched Linux binary across arbitrary distros (glibc/ABI
  + WebKit/GTK runtime constraints; see §6).

## 3. Architecture

Two workstreams, **A before B** (B reuses A's single-build command).

```
Workstream A (local, platform-agnostic xtask fixes)
  xtask/src/main.rs
    - single `cargo build --release -p spur-cli -p spur-notebook
        --features spur-notebook/custom-protocol --locked`  (shared target/)
    - copy target/release/{spur,spur-notebook} -> $CARGO_HOME/bin
    - RUSTC_WRAPPER strip gated to cfg!(target_os = "macos")
    - macOS: drop redundant ensure_jute_frontend_dist (tauri build owns it)

Workstream B (remote install path)
  startup.sh   : provision Node LTS + Corepack on the VM
  build.sh     : when building spur-notebook w/ custom-protocol, run the
                 frontend build (npm ci && npm run build in jute-notebook)
                 BEFORE cargo, so dist/ exists for generate_context!
  xtask        : `install --remote` -> dispatch the single build to the VM via
                 build.sh, then fetch.sh pulls the two release binaries back
  fetch.sh     : fetch target/release/{spur,spur-notebook}
```

## 4. Decisions

- **D1 — single `cargo build` + copy** (vs two `cargo install` sharing a target
  dir). Chosen: single build + copy. Simpler, mirrors how `target/release` already
  feeds `fetch.sh`, and gives one code path local+remote. **Tradeoff:** bypasses
  `cargo install`'s `.crates.toml` tracking, so uninstall/upgrade becomes xtask's
  responsibility (codex footgun #4). Mitigation: xtask removes the prior binaries
  before copying; document that `cargo uninstall spur`/`spur-notebook` will not see
  them.
- **D2 — stay npm now.** Main uses npm (`package-lock.json`,
  `beforeBuildCommand: "npm run build"`); pnpm exists only in an unmerged worktree.
  Keep npm and isolate the frontend command to a single configurable location so
  the pnpm migration (Corepack + `--frozen-lockfile`, updating `tauri.conf.json` +
  xtask + build scripts together, per codex) is a later one-line change. Provision
  Corepack on the VM regardless so the pnpm switch needs no re-provisioning.
- **D3 — remote opt-in via `--remote` flag.** `cargo xtask install` stays local;
  `cargo xtask install --remote` offloads the Linux build. Keeps the default
  behavior unchanged.

## 5. Components & changes

### 5.1 `xtask/src/main.rs` (workstream A)

The key to killing the double compile on **both** platforms is that everything
builds into the **shared workspace `target/`** (which both `cargo build` and
`tauri build` already use), so the shared dep tree compiles once.

- **Linux path:** single `cargo build --release -p spur-cli -p spur-notebook
  --features spur-notebook/custom-protocol --locked` into `target/`, then copy
  `target/release/{spur,spur-notebook}` → `$CARGO_HOME/bin` (remove existing
  first). One dep-tree compile. `ensure_jute_frontend_dist` runs before the cargo
  build (the direct-`cargo build` path does not run `beforeBuildCommand`).
- **macOS path:** build `spur-cli` via `cargo build --release -p spur-cli` into
  `target/` + copy (replacing the old `cargo install` temp-dir build), then
  `tauri build` for the `.app` (also `target/release`). Because both use the same
  `target/`, the shared deps compile **once** on macOS too. Drop the redundant
  explicit `ensure_jute_frontend_dist` before `tauri build` — `beforeBuildCommand`
  owns the frontend build on this path.
- **Both platforms:** gate `RUSTC_WRAPPER` removal behind
  `cfg!(target_os = "macos")`. On Linux, leave the caller's wrapper intact so the
  VM's sccache applies; macOS keeps the strip for the `com.apple.provenance`
  collision.
- Update unit tests (`cargo_install_command_includes_requested_features`,
  `tauri_build_command_runs_outer_spur_notebook_crate`) to match the new shape.

### 5.2 `scripts/gcp-build/startup.sh` (workstream B)
- Install Node LTS + enable Corepack (idempotent, gated like the other tool
  installs). Frozen-lockfile installs only.
- Mirror the GTK/WebKit/clang/cmake deps already present (no change; they exist).

### 5.3 `scripts/gcp-build/build.sh` (workstream B)
- Detect a "notebook production build" (cargo args include `spur-notebook` +
  `custom-protocol`). When detected, run `npm ci && npm run build` in
  `crates/spur-notebook/jute-notebook` on the VM **before** cargo, so `dist/`
  exists. Normal `check/test/clippy` loops are unaffected (gate off the feature
  flag / package selector).
- `dist/` is built on the VM, not synced (it stays gitignored).

### 5.4 `scripts/gcp-build/fetch.sh` (workstream B)
- Add a mode to fetch `target/release/spur` and `target/release/spur-notebook`
  from the worktree's remote target dir back to `$CARGO_HOME/bin`.

### 5.5 `cargo xtask install --remote` (workstream B)
- Parse `--remote`. When set (non-macOS host, or explicitly): dispatch the single
  workspace build to the VM via `build.sh`, then run `fetch.sh` to install the two
  binaries locally. Print the same sibling-verification as the local path
  (`verify_sibling_install`).

## 6. Risks / footguns (from codex)

- **`custom-protocol` = distinct sccache cache variant.** Every production build
  must set it consistently or it fragments the cache / silently builds dev mode.
- **glibc/ABI lock-in.** Debian-12-built binaries require ≥ that glibc; fine for
  matching dev boxes, out of scope for wide distribution.
- **`TAURI_CONFIG` externalBin suppression** (`.cargo/config.toml` sets
  `{"bundle":{"externalBin":[]}}`). Verify it does not bleed into release context
  generation for the production build.
- **`.crates.toml` bypass** (D1): xtask owns uninstall/upgrade semantics.
- **Tauri Linux runtime deps.** The fetched binary still needs WebKit/GTK present
  at runtime.
- **`generate_context!` + missing/stale `dist`.** Any direct production `cargo
  build` must build `jute-notebook/dist` first (handled by §5.3 remotely, by
  `ensure_jute_frontend_dist` locally).

## 7. Testing

- **A:** unit tests for the new build/copy command shape (assert single
  `cargo build` with `custom-protocol`, assert macOS-only `RUSTC_WRAPPER` strip,
  assert no double frontend build on macOS). Manual: `cargo xtask install` on
  Linux + macOS produces working `spur` + notebook.
- **B:** manual end-to-end — `cargo xtask install --remote` on a Linux host:
  VM provisions Node, builds frontend once, single workspace build with high
  sccache hit rate, fetch installs both binaries; launched notebook renders the
  real frontend (not the Tauri placeholder/blank page).

## 8. Build sequence (for the plan)

1. Workstream A — xtask single-build + copy, macOS-only wrapper strip, remove
   macOS double-frontend-build, update tests. (Local, verifiable without the VM.)
2. Workstream B.1 — `startup.sh` Node/Corepack provisioning; re-apply to VM.
3. Workstream B.2 — `build.sh` frontend build step (feature-gated).
4. Workstream B.3 — `fetch.sh` binary-fetch mode + `xtask install --remote`.
5. End-to-end remote install validation; document glibc/runtime constraints.
