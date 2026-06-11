#!/usr/bin/env bash
# sccache-worktree.sh
#
# rustc-wrapper that dynamically sets SCCACHE_BASEDIRS to the current git
# worktree root so sccache normalizes workspace paths identically across all
# worktrees and the main repo. Local macOS builds can opt into the shared GCS
# backend with SPUR_SCCACHE_GCS=1.
#
# Why: sccache 0.14.0 strips SCCACHE_BASEDIRS prefixes before hashing.
# Without this wrapper, each worktree's unique subdirectory name remains in the
# relative path, causing identical source files to hash differently.
#
# See: docs/rca/2026-04-27-sccache-worktree-cache-miss.md
set -euo pipefail

# Resolve the git toplevel of the current working directory. In a git worktree
# this is the worktree root; in the main repo it is the repo root.
GIT_ROOT=""
if command -v git >/dev/null 2>&1; then
    GIT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo "")
fi

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="${SCRIPT_PATH%/*}"
if [[ "$SCRIPT_DIR" == "$SCRIPT_PATH" ]]; then
    SCRIPT_DIR="."
fi

# Always include the main SPUR repo root as a fallback so registry caches and
# shared artifacts normalize consistently.
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd -P)
SPUR_ROOT="${SPUR_ROOT:-$REPO_ROOT}"

enable_spur_gcs_cache() {
    [[ "${SPUR_SCCACHE_GCS:-0}" == "1" ]] || return 0

    local platform
    platform=$(uname -s 2>/dev/null || echo "")
    if [[ "$platform" != "Darwin" && "${SPUR_SCCACHE_GCS_FORCE:-0}" != "1" ]]; then
        return 0
    fi

    local project bucket
    project="${GCP_PROJECT:-wiilearn}"
    bucket="${SCCACHE_BUCKET:-${project}-spur-sccache-asia}"

    if [[ -z "${SCCACHE_GCS_BUCKET:-}" ]]; then
        export SCCACHE_GCS_BUCKET="$bucket"
    fi

    if [[ -n "${SCCACHE_GCS_BUCKET:-}" ]]; then
        export SCCACHE_GCS_RW_MODE="${SCCACHE_GCS_RW_MODE:-READ_WRITE}"
        # sccache 0.15+ can use disk,gcs as a real multi-level chain. Older
        # sccache builds ignore this var and use GCS as the single configured
        # backend when SCCACHE_GCS_BUCKET is set.
        export SCCACHE_MULTILEVEL_CHAIN="${SCCACHE_MULTILEVEL_CHAIN:-disk,gcs}"
    fi
}

if [[ -n "$GIT_ROOT" && "$GIT_ROOT" != "$SPUR_ROOT" ]]; then
    # Worktree: strip to the worktree root first (longest-prefix wins).
    export SCCACHE_BASEDIRS="${GIT_ROOT}:${SPUR_ROOT}"
else
    # Main repo or not inside git: just the repo root.
    export SCCACHE_BASEDIRS="${SPUR_ROOT}"
fi

enable_spur_gcs_cache

IS_SCCACHE_CONTROL=0
case "${1:-}" in
    --show-stats|--show-adv-stats|--start-server|--stop-server|--zero-stats|\
    --dist-status|--dist-auth|--debug-preprocessor-cache|--package-toolchain)
        IS_SCCACHE_CONTROL=1 ;;
esac

if [[ -n "${CODEX_SANDBOX:-}" && "$IS_SCCACHE_CONTROL" -eq 0 ]]; then
    exec "$@"
fi

if command -v sccache >/dev/null 2>&1; then
    exec sccache "$@"
fi

exec "$@"
