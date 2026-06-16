# SPUR Subcrate: Jute Notebook

This directory is the Jute-derived frontend subcrate for `spur-notebook`, tracked in the [getspur/spur](https://github.com/getspur/spur) repository at `crates/spur-notebook/jute-notebook/`. It powers the SPUR Notebook frontend (see `docs/superpowers/plans/spur-notebook-v0.4-build-plan.md`).

The initial source came from [ekzhang/jute](https://github.com/ekzhang/jute). Keep the upstream remote available as `jute` for provenance, but SPUR-local development, issues, and package metadata belong to `getspur/spur`.

## Pin

| Field | Value |
|---|---|
| Upstream commit | `18723a036b843d9efc1d07b326bda5614b2020e7` |
| Imported at | 2026-05-22 |
| SPUR history | initial prefixed subtree merge plus a history-only merge from `jute/main`, so upstream Jute commits are reachable from SPUR history |
| License      | MIT (preserved in-tree at `LICENSE`) |

## Initial import command

```sh
git remote add jute https://github.com/ekzhang/jute
git fetch jute

# Subtree add must run with a clean working tree.
# If your tree has unrelated modifications, do it in a detached worktree:
git worktree add --detach /tmp/spur-jute-subtree HEAD
( cd /tmp/spur-jute-subtree
  git switch -c subtree-add-jute
  git subtree add --prefix=crates/spur-notebook/jute-notebook jute main
)
git merge --ff-only subtree-add-jute
git worktree remove /tmp/spur-jute-subtree
git branch -D subtree-add-jute
```

The historical import used a squash subtree first, then joined the upstream commit graph with a history-only merge. Verify that join with:

```sh
git merge-base --is-ancestor jute/main HEAD
```

## Updating from upstream

```sh
git fetch jute
git subtree pull --prefix=crates/spur-notebook/jute-notebook jute main
# Same dirty-worktree caveat — use a worktree if needed (see above).
```

Update this file's pin (commit SHA + date) on every successful pull. Conflicts are resolved in-place; do not push back to upstream.

## Caveats

- **Nested layout (Amendment B).** This crate lives at `crates/spur-notebook/jute-notebook/` rather than the conventional flat `crates/jute-notebook/`. If the nested layout causes Cargo/Vite/Tailwind/Tauri tooling friction at any point in M1, fall back to the flat top-level layout (`crates/jute-notebook/`, peer of `crates/spur-notebook/`) and update the workspace `members` + this file's prefix.
- **Nested subcrate layout.** `src-tauri` is the Rust `jute` package and is a workspace member; the React package at this directory is tracked as the frontend package for `spur-notebook`.
- **CI exclusions.** Jute's upstream `.github/` workflows are vendored along with the source. Treat them as documentation; they are not run in SPUR CI.
- **The `experiment/` subdirectory** is upstream Python scratch (uv-managed venv for Jupyter wire-protocol R&D). Not built or run by SPUR CI; left in-tree for parity with upstream.

## SPUR drift

- `src-tauri/src/backend/notebook.rs`: added typed `metadata.spur.version` per-cell metadata so SPUR's optimistic cell version survives `.ipynb` parse/serialize.
- `src-tauri/src/commands.rs`, `src-tauri/src/state.rs`, `../src/main.rs`: added the `save_to_disk` Tauri command, process-local save coalescing, and same-directory atomic temp-file rename for autosave.
- `src-tauri/src/commands.rs`, `src-tauri/src/state.rs`, `../src/main.rs`: replaced upstream's per-start local kernel ID map with stable notebook path-derived kernel slots and in-memory slot generations, while keeping the existing JS-facing command argument names compatible.
- `src/stores/notebook.ts`, `src/ui/notebook/CellInput.tsx`: made Zustand track per-cell `source` and monotonic `version`, bumped versions on source/type edits, generated UUIDv4 cell ids on insert, and wired 5 second debounced autosave.
- `src-tauri/src/bin/ts-rs-export.rs`: resolves generated TypeScript bindings from the vendored app root so `scripts/spur-cargo run -p jute --bin ts-rs-export` can be rerun from the SPUR workspace.

Verified zero TypeScript binding drift on 2026-05-25 after regenerating with `scripts/spur-cargo run -p jute --bin ts-rs-export`.

CI can verify generated bindings with:

```sh
npm --prefix crates/spur-notebook/jute-notebook ci
scripts/spur-cargo run -p jute --bin ts-rs-export
git diff --exit-code -- crates/spur-notebook/jute-notebook/src/bindings/
```

## SPUR lifecycle notes

- The daemon persists the last loaded notebook in `~/.spur/notebooks/last.json` and attempts to restore that single notebook on restart. `close` clears the record.
- Multi-window remains deferred for v0.4. The daemon, stable MCP socket, kernel slot model, and `last.json` restore path all assume one active notebook window.
- Autosave keeps Jute's 5 second JS debounce and Rust-side same-directory temp-file plus atomic rename. A JS panic during the debounce window can lose the pending edit, but the on-disk `.ipynb` remains a complete old or new notebook.
