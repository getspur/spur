# SPUR Subcrate: Jute Notebook

This directory is the Jute-derived frontend subcrate for `spur-notebook`, tracked in the [getspur/spur](https://github.com/getspur/spur) repository at `crates/spur-notebook/jute-notebook/`. It powers the SPUR Notebook frontend (see `docs/superpowers/plans/spur-notebook-v0.4-build-plan.md`).

The initial source came from the external Jute project. That source history is already joined into SPUR's Git ancestry, so `getspur/spur` is the only configured upstream for ongoing Jute Notebook development.

## Pin

| Field | Value |
|---|---|
| Imported source commit | `18723a036b843d9efc1d07b326bda5614b2020e7` |
| Imported at | 2026-05-22 |
| SPUR history | initial prefixed subtree merge plus a history-only merge from the imported source commit, so the original Jute commits are reachable from SPUR history |
| License      | MIT (preserved in-tree at `LICENSE`) |

## Initial import command

The historical import used a squash subtree first, then joined the original Jute commit graph with a history-only merge. Verify that join with:

```sh
git merge-base --is-ancestor 18723a036b843d9efc1d07b326bda5614b2020e7 HEAD
```

## Maintenance

Do not configure a separate Jute remote for this subcrate. Make changes directly under `crates/spur-notebook/jute-notebook/` in `getspur/spur`, and route issues, reviews, and package metadata through SPUR.

## Caveats

- **Nested layout (Amendment B).** This crate lives at `crates/spur-notebook/jute-notebook/` rather than the conventional flat `crates/jute-notebook/`. If the nested layout causes Cargo/Vite/Tailwind/Tauri tooling friction at any point in M1, fall back to the flat top-level layout (`crates/jute-notebook/`, peer of `crates/spur-notebook/`) and update the workspace `members` + this file's prefix.
- **Nested subcrate layout.** `src-tauri` is the Rust `jute` package and is a workspace member; the React package at this directory is tracked as the frontend package for `spur-notebook`.
- **CI exclusions.** Jute's imported `.github/` workflows are preserved with the source. Treat them as documentation; they are not run in SPUR CI.
- **The `experiment/` subdirectory** is imported Python scratch (uv-managed venv for Jupyter wire-protocol R&D). Not built or run by SPUR CI; left in-tree for source parity.

## SPUR drift

- `src-tauri/src/backend/notebook.rs`: added typed `metadata.spur.version` per-cell metadata so SPUR's optimistic cell version survives `.ipynb` parse/serialize.
- `src-tauri/src/commands.rs`, `src-tauri/src/state.rs`, `../src/main.rs`: added the `save_to_disk` Tauri command, process-local save coalescing, and same-directory atomic temp-file rename for autosave.
- `src-tauri/src/commands.rs`, `src-tauri/src/state.rs`, `../src/main.rs`: replaced the original per-start local kernel ID map with stable notebook path-derived kernel slots and in-memory slot generations, while keeping the existing JS-facing command argument names compatible.
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
