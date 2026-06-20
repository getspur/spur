#!/usr/bin/env bash
# sccache-worktree.sh
#
# rustc-wrapper that dynamically sets SCCACHE_BASEDIRS to the current git
# worktree root so sccache normalizes workspace paths identically across all
# worktrees and the main repo. Local builds default to a shared remote backend:
#   default             → two-level cache L0=local disk, L1=AWS S3
#                         (SCCACHE_MULTILEVEL_CHAIN=disk,s3). Default bucket
#                         wiilearn-spur-sccache-apne1 in ap-northeast-1.
#   SPUR_SCCACHE_S3=0  → disable the default S3 backend.
#   SPUR_SCCACHE_GCS=1 → two-level cache L0=local disk, L1=GCS (macOS-gated)
#                         when SPUR_SCCACHE_S3 is unset or disabled.
# Explicit S3 takes precedence when both are set. Each remote backend restarts
# the sccache server via spur-cargo so the daemon picks up the multilevel config.
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

use_spur_s3_sccache() {
    case "${SPUR_SCCACHE_S3-__spur_unset__}" in
        0|false|False|FALSE|no|No|NO|off|Off|OFF)
            return 1 ;;
        __spur_unset__)
            [[ "${SPUR_SCCACHE_GCS:-0}" == "1" ]] && return 1
            return 0 ;;
        *)
            return 0 ;;
    esac
}

# Two-level cache: L0=local disk (fast), L1=AWS S3 (shared, durable). sccache
# 0.15+ implements this natively via SCCACHE_MULTILEVEL_CHAIN; on an L1 hit it
# backfills L0. Returns 0 when activated, 1 when the caller can fall through to
# the GCS path.
enable_spur_s3_cache() {
    use_spur_s3_sccache || return 1

    # L1: AWS S3. SCCACHE_REGION MUST match the bucket's region or S3 rejects
    # the request. Bucket suffix apne1 == ap-northeast-1 (Tokyo).
    export SCCACHE_BUCKET="${SCCACHE_BUCKET:-wiilearn-spur-sccache-apne1}"
    export SCCACHE_REGION="${SCCACHE_REGION:-ap-northeast-1}"
    export AWS_REGION="${AWS_REGION:-$SCCACHE_REGION}"
    # Credentials resolve through the standard AWS chain (env vars, then the
    # default profile in ~/.aws/credentials, then IMDS).

    # L0: local disk. The `disk` level REQUIRES SCCACHE_DIR to be set explicitly
    # ("Disk cache specified in levels but not configured"), so default it to the
    # platform cache dir sccache already uses to reuse any existing local cache.
    if [[ -z "${SCCACHE_DIR:-}" ]]; then
        case "$(uname -s 2>/dev/null || echo "")" in
            Darwin) export SCCACHE_DIR="$HOME/Library/Caches/Mozilla.sccache" ;;
            *)      export SCCACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/sccache" ;;
        esac
    fi
    export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-10G}"

    export SCCACHE_MULTILEVEL_CHAIN="${SCCACHE_MULTILEVEL_CHAIN:-disk,s3}"
    # l0: only an L0 write failure is fatal; tolerate transient S3 write errors
    # so a flaky network never reds a build.
    export SCCACHE_MULTILEVEL_WRITE_ERROR_POLICY="${SCCACHE_MULTILEVEL_WRITE_ERROR_POLICY:-l0}"
    return 0
}

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

# S3 takes precedence; fall through to GCS only when S3 is not requested.
enable_spur_s3_cache || enable_spur_gcs_cache

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
