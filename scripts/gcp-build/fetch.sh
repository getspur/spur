#!/usr/bin/env bash
# Fetch an artifact from the VM's per-worktree target dir to the local repo.
#
# Auto-detects worktree (main vs .spur/worktrees/<uuid>) from cwd, so call from
# inside the worktree whose binary you want.
#
# Usage:
#   ./fetch.sh target/release/spur                  # → ./target/release/spur (relative to worktree)
#   ./fetch.sh target/debug/deps/foo-abc            # any path under target/
#   ./fetch.sh --to /tmp/spur target/release/spur   # explicit local dest
#   ./fetch.sh --bins                               # → ${CARGO_HOME:-$HOME/.cargo}/bin/{spur,spur-notebook}
#   ./fetch.sh --bins --to /tmp/bin                 # explicit local bin dir
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[fetch] $*" >&2; }
usage() {
    echo "usage: fetch.sh [--to <local>] <target-relative-path>" >&2
    echo "       fetch.sh --bins [--to <local-bin-dir>]" >&2
}

LOCAL_DEST=""
FETCH_BINS=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --to)
            [[ $# -ge 2 ]] || { usage; exit 2; }
            LOCAL_DEST="$2"
            shift 2
            ;;
        --bins)
            FETCH_BINS=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            break
            ;;
        -*)
            usage
            exit 2
            ;;
        *)
            break
            ;;
    esac
done

GIT_TOPLEVEL=$(git rev-parse --show-toplevel 2>/dev/null || echo "")
[[ -z "$GIT_TOPLEVEL" ]] && { log "not inside a git repo"; exit 2; }

if [[ "$GIT_TOPLEVEL" == *"/.spur/worktrees/"* ]]; then
    WORKTREE_KEY="worktrees/$(basename "$GIT_TOPLEVEL")"
else
    WORKTREE_KEY="main"
fi

target_remote_path() {
    local remote_rel="$1"
    # Strip a leading "target/" to compute the path under /mnt/cargo/targets/<key>/
    if [[ "$remote_rel" == target/* ]]; then
        echo "/mnt/cargo/targets/$WORKTREE_KEY/${remote_rel#target/}"
    else
        echo "/mnt/cargo/targets/$WORKTREE_KEY/$remote_rel"
    fi
}

fetch_one() {
    local remote_rel="$1"
    local local_dest="$2"
    local remote_path
    remote_path=$(target_remote_path "$remote_rel")

    log "Worktree: $WORKTREE_KEY"
    log "Remote:   $VM_NAME:$remote_path"
    log "Local:    $local_dest"

    gcloud compute scp \
        --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
        --tunnel-through-iap \
        --recurse \
        "$VM_NAME:$remote_path" "$local_dest"
}

if [[ $FETCH_BINS -eq 1 ]]; then
    [[ $# -eq 0 ]] || { usage; exit 2; }
    BIN_DEST="${LOCAL_DEST:-${CARGO_HOME:-$HOME/.cargo}/bin}"
    mkdir -p "$BIN_DEST"

    for bin in spur spur-notebook; do
        fetch_one "target/release/$bin" "$BIN_DEST/$bin"
        chmod 0755 "$BIN_DEST/$bin"
    done

    log "Done."
    exit 0
fi

[[ $# -eq 1 ]] || { usage; exit 2; }
REMOTE_REL="$1"

[[ -z "$LOCAL_DEST" ]] && LOCAL_DEST="$GIT_TOPLEVEL/$REMOTE_REL"
mkdir -p "$(dirname "$LOCAL_DEST")"
fetch_one "$REMOTE_REL" "$LOCAL_DEST"

log "Done."
