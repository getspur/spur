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
        echo "[prune] removing $(wc -l <"$gone" | tr -d ' ') file(s) deleted locally since last sync:"
        sed 's/^/  - /' "$gone"
        while IFS= read -r rel; do
            [ -n "$rel" ] || continue
            # Defensive: never touch the target/ build-output symlink tree.
            case "$rel" in
                target | target/*) continue ;;
            esac
            rm -f -- "$workdir/$rel"
        done <"$gone"
        # Drop directories left empty by the prune (never descend into target/).
        ( cd "$workdir" && find . -path ./target -prune -o -type d -empty -delete 2>/dev/null || true )
    else
        echo "[prune] no files removed locally since last sync"
    fi
else
    echo "[prune] no prior manifest for this worktree; seeding baseline (no prune on first run)"
fi

# Persist the current manifest as the baseline for the next sync.
mkdir -p "$(dirname "$stored")"
cp -f "$current" "$stored"
