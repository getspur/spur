#!/usr/bin/env bash
# Reconcile the remote build workspace by deleting source files that were synced
# in a previous run but have since been removed or renamed locally.
#
# Why this exists
# ---------------
# build.sh syncs the worktree to the VM with:
#     rsync -az --delete -0 --files-from=<git ls-files> src/ vm:dst/
# but rsync's --delete is INERT when combined with --files-from: rsync transfers
# only the explicitly listed files and never recurses the destination, so it
# cannot enumerate (and therefore cannot prune) extraneous files. A file removed
# locally thus lingers on the VM forever, silently desyncing the remote build
# from the local tree (stale modules, removed/renamed tests, etc.).
#
# Strategy
# --------
# Prune exactly (previous manifest - current manifest). This deletes only files
# that rsync itself placed in an earlier sync and that are now gone locally. It
# NEVER touches VM-generated artifacts (node_modules/, dist/, target/, generated
# schemas), because those were never present in any manifest. On the first run
# for a worktree there is no prior manifest, so nothing is pruned — we simply
# seed the baseline.
#
# Hardening (2026-07-15)
# ----------------------
# Manifests are file/symlink leaves only (`git ls-files`). A path can still land
# as a *directory* on the VM when a prior leaf (commonly a symlink such as
# `scripts/gcp-build` → dir, or a file that became a dir) was replaced by a
# directory whose children are still in the current manifest. `rm -f` on a
# directory fails with "Is a directory" and, under `set -e`, used to abort the
# entire remote build before cargo ever ran — observed with 2000+ stale
# notebook paths in a polluted `spur/main` stored manifest, dying on
# `scripts/gcp-build`. Rules:
#   - never abort the build over a single path
#   - only `rm -f` files/symlinks
#   - for directories: `rmdir` if empty (non-empty means children still live);
#     never `rm -rf` a directory that may still hold current-manifest files
#   - always persist the current manifest so a partial prune still converges
#
# Note: manifests are git ls-files -z output (NUL-separated). We compare via
# newline-delimited sort/comm; paths containing literal newlines (which git
# permits but this repo never uses) are out of scope.
#
# Usage:
#   _prune-remote.sh <remote_dir_rel_home> <current_manifest> <stored_manifest>
set -euo pipefail

remote_dir="${1:?remote dir (relative to \$HOME) required}"
current="${2:?current manifest path required}"
stored="${3:?stored manifest path required}"

workdir="$HOME/$remote_dir"

# Remove one gone path. Never exits non-zero for path-level problems.
prune_one() {
    local rel="$1"
    local target

    [ -n "$rel" ] || return 0

    # Refuse path traversal / absolute paths / the target symlink tree.
    case "$rel" in
        /* | *..* | target | target/*)
            echo "[prune] skip unsafe path: $rel" >&2
            return 0
            ;;
    esac

    target="$workdir/$rel"

    if [ -L "$target" ]; then
        # Symlink leaf (may point at a directory). rm -f removes the link only.
        rm -f -- "$target" 2>/dev/null || true
        return 0
    fi

    if [ -f "$target" ]; then
        rm -f -- "$target" 2>/dev/null || true
        return 0
    fi

    if [ -d "$target" ]; then
        # Prior manifest listed this as a leaf that is now a directory on the
        # VM (symlink→dir or file→dir transition), or a polluted dir entry.
        # Only remove if empty; non-empty means current-manifest children still
        # live underneath (must not recursive-delete).
        if rmdir -- "$target" 2>/dev/null; then
            return 0
        fi
        echo "[prune] skip non-empty directory (children may still be live): $rel" >&2
        return 0
    fi

    # Already gone — fine.
    return 0
}

if [ -f "$stored" ]; then
    cur_sorted="$(mktemp)"
    prev_sorted="$(mktemp)"
    gone="$(mktemp)"
    trap 'rm -f "$cur_sorted" "$prev_sorted" "$gone"' EXIT

    tr '\0' '\n' <"$current" | LC_ALL=C sort -u >"$cur_sorted"
    tr '\0' '\n' <"$stored" | LC_ALL=C sort -u >"$prev_sorted"

    # Files present in the previous sync but not the current one.
    comm -23 "$prev_sorted" "$cur_sorted" >"$gone"

    if [ -s "$gone" ]; then
        gone_count=$(wc -l <"$gone" | tr -d ' ')
        echo "[prune] removing $gone_count file(s) deleted locally since last sync:"
        # Cap log spam on huge pollutions (e.g. monorepo→split leftover manifests).
        if [ "$gone_count" -gt 50 ]; then
            sed -n '1,40p' "$gone" | sed 's/^/  - /'
            echo "  ... ($((gone_count - 40)) more)"
        else
            sed 's/^/  - /' "$gone"
        fi
        while IFS= read -r rel; do
            prune_one "$rel"
        done <"$gone"
        # Drop directories left empty by the prune (never descend into target/).
        ( cd "$workdir" && find . -path ./target -prune -o -type d -empty -delete 2>/dev/null || true )
    else
        echo "[prune] no files removed locally since last sync"
    fi
else
    echo "[prune] no prior manifest for this worktree; seeding baseline (no prune on first run)"
fi

# Persist the current manifest as the baseline for the next sync — always,
# even when individual path removals were skipped, so the next run converges.
mkdir -p "$(dirname "$stored")"
cp -f "$current" "$stored"
