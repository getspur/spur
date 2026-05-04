#!/usr/bin/env bash
# sccache-sync-basedirs.sh
#
# DEPRECATED: Superseded by `scripts/sccache-worktree.sh`, which is injected by
# `scripts/spur-cargo`. The wrapper dynamically sets SCCACHE_BASEDIRS per
# invocation, eliminating the need to enumerate all worktrees and restart the
# server.
#
# Kept for reference and emergency fallback only.
#
# Original purpose: Sync the sccache server's `SCCACHE_BASEDIRS` to the
# current set of git worktree roots under SPUR_ROOT.
#
# See: docs/rca/2026-04-27-sccache-worktree-cache-miss.md
set -euo pipefail

SPUR_ROOT="${SPUR_ROOT:-/Volumes/Projects/spur}"
LOCK_DIR="/tmp/sccache-sync-basedirs.lockd"
QUIET="${SCCACHE_SYNC_QUIET:-0}"

log() { [[ "$QUIET" == "1" ]] || echo "[sccache-sync] $*" >&2; }

# Atomic single-flight lock (portable; no flock dependency).
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    exit 0
fi
trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT INT TERM

# Enumerate current worktree roots. Each root + each parent + main repo.
# Longest-prefix matching means specific roots win when both are listed.
BASEDIRS=()
shopt -s nullglob
for d in "$SPUR_ROOT/.worktrees"/*/ "$SPUR_ROOT/.spur/worktrees"/*/; do
    BASEDIRS+=("${d%/}")
done
BASEDIRS+=("$SPUR_ROOT/.worktrees" "$SPUR_ROOT/.spur/worktrees" "$SPUR_ROOT")

NEW_BASEDIRS=$(IFS=:; echo "${BASEDIRS[*]}")

# Read the running server's basedirs (if any). sccache prints them as
# "Base directories  /a/, /b/, /c/".
CURRENT=$(sccache --show-stats 2>/dev/null \
    | awk '/^Base directories/ {sub(/^Base directories[[:space:]]+/, ""); print}' \
    | tr ',' ':' | tr -d ' ' || true)

# Normalize for comparison: strip trailing slashes, sort.
norm() {
    local v="${1:-}"
    [[ -z "$v" ]] && { echo ""; return; }
    echo "$v" | tr ':' '\n' | sed 's|/$||' | sort -u | paste -sd: -
}

NORM_CURRENT=$(norm "$CURRENT")
NORM_NEW=$(norm "$NEW_BASEDIRS")

if [[ "$NORM_CURRENT" == "$NORM_NEW" ]]; then
    log "basedirs already in sync (${#BASEDIRS[@]} entries)"
    exit 0
fi

# Active-build guard: refuse to restart if rustc invocations are happening,
# to avoid breaking in-flight compiles.
if pgrep -f '/rustc[^ ]*$|/rustc ' >/dev/null 2>&1; then
    log "rustc is running — skipping restart to avoid breaking the build"
    exit 0
fi

log "drift detected — restarting sccache server with ${#BASEDIRS[@]} basedirs"

sccache --stop-server >/dev/null 2>&1 || true

# Wait for the listening socket to actually free (macOS holds it briefly).
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if ! lsof -i :4226 >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# Unset other sccache env vars so the persistent config file wins for
# everything except basedirs (which we set explicitly here). Stale shells
# may still have SCCACHE_CACHE_SIZE etc. exported; that would silently
# override file config and reduce cache size on restart.
env -u SCCACHE_CACHE_SIZE -u SCCACHE_DIR -u SCCACHE_CONF \
    SCCACHE_BASEDIRS="$NEW_BASEDIRS" \
    sccache --start-server

log "server up. New basedirs:"
[[ "$QUIET" == "1" ]] || sccache --show-stats | awk '/^Base directories/ {print}' >&2
