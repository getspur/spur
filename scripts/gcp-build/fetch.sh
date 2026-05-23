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
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[fetch] $*" >&2; }

LOCAL_DEST=""
if [[ "${1:-}" == "--to" ]]; then
    LOCAL_DEST="$2"; shift 2
fi
REMOTE_REL="${1:?usage: fetch.sh [--to <local>] <target-relative-path>}"

GIT_TOPLEVEL=$(git rev-parse --show-toplevel 2>/dev/null || echo "")
[[ -z "$GIT_TOPLEVEL" ]] && { log "not inside a git repo"; exit 2; }

if [[ "$GIT_TOPLEVEL" == *"/.spur/worktrees/"* ]]; then
    WORKTREE_KEY="worktrees/$(basename "$GIT_TOPLEVEL")"
else
    WORKTREE_KEY="main"
fi

# Strip a leading "target/" to compute the path under /mnt/cargo/targets/<key>/
if [[ "$REMOTE_REL" == target/* ]]; then
    REMOTE_PATH="/mnt/cargo/targets/$WORKTREE_KEY/${REMOTE_REL#target/}"
else
    REMOTE_PATH="/mnt/cargo/targets/$WORKTREE_KEY/$REMOTE_REL"
fi

[[ -z "$LOCAL_DEST" ]] && LOCAL_DEST="$GIT_TOPLEVEL/$REMOTE_REL"
mkdir -p "$(dirname "$LOCAL_DEST")"

log "Worktree: $WORKTREE_KEY"
log "Remote:   $VM_NAME:$REMOTE_PATH"
log "Local:    $LOCAL_DEST"

gcloud compute scp \
    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
    --tunnel-through-iap \
    --recurse \
    "$VM_NAME:$REMOTE_PATH" "$LOCAL_DEST"

log "Done."
