# Vendored: ekzhang/jute

This directory is a vendored copy of [ekzhang/jute](https://github.com/ekzhang/jute), imported via `git subtree --squash`. It powers the SPUR Notebook frontend (see `docs/superpowers/plans/spur-notebook-v0.4-build-plan.md`).

## Pin

| Field | Value |
|---|---|
| Upstream commit | `18723a036b843d9efc1d07b326bda5614b2020e7` |
| Imported at | 2026-05-22 |
| SPUR commit  | recorded by the `Squashed 'crates/spur-notebook/jute-notebook/' content from commit 18723a03` parent in `git log` |
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
  git subtree add --prefix=crates/spur-notebook/jute-notebook jute main --squash
)
git merge --ff-only subtree-add-jute
git worktree remove /tmp/spur-jute-subtree
git branch -D subtree-add-jute
```

## Updating from upstream

```sh
git fetch jute
git subtree pull --prefix=crates/spur-notebook/jute-notebook jute main --squash
# Same dirty-worktree caveat — use a worktree if needed (see above).
```

Update this file's pin (commit SHA + date) on every successful pull. Conflicts are resolved in-place; do not push back to upstream.

## Caveats

- **Nested layout (Amendment B).** This crate lives at `crates/spur-notebook/jute-notebook/` rather than the conventional flat `crates/jute-notebook/`. If the nested layout causes Cargo/Vite/Tailwind/Tauri tooling friction at any point in M1, fall back to the flat top-level layout (`crates/jute-notebook/`, peer of `crates/spur-notebook/`) and update the workspace `members` + this file's prefix.
- **No SPUR dependencies.** This crate must not depend on any `spur-*` crate. Dependency direction is one-way: `spur-notebook` depends on `jute-notebook`, never the reverse.
- **CI exclusions.** Jute's upstream `.github/` workflows are vendored along with the source. Treat them as documentation; they are not run in SPUR CI.
- **The `experiment/` subdirectory** is upstream Python scratch (uv-managed venv for Jupyter wire-protocol R&D). Not built or run by SPUR CI; left in-tree for parity with upstream.
