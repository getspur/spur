#!/usr/bin/env bash
# sccache-worktree.sh
#
# rustc-wrapper that dynamically sets SCCACHE_BASEDIRS to the current git
# worktree root so sccache normalizes workspace paths identically across all
# worktrees and the main repo.
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

if [[ -n "$GIT_ROOT" && "$GIT_ROOT" != "$SPUR_ROOT" ]]; then
    # Worktree: strip to the worktree root first (longest-prefix wins).
    export SCCACHE_BASEDIRS="${GIT_ROOT}:${SPUR_ROOT}"
else
    # Main repo or not inside git: just the repo root.
    export SCCACHE_BASEDIRS="${SPUR_ROOT}"
fi

if [[ -n "${CODEX_SANDBOX:-}" ]]; then
    exec "$@"
fi

if command -v sccache >/dev/null 2>&1; then
    exec sccache "$@"
fi

exec "$@"
