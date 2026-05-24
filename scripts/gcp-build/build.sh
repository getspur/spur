#!/usr/bin/env bash
# Sync the current worktree to the build VM and run cargo remotely.
#
# Multi-worktree aware: detects whether we're in the main repo or a
# .spur/worktrees/<UUID> worktree, syncs to a matching remote dir, and uses
# per-worktree CARGO_TARGET_DIR + SCCACHE_BASEDIRS so all worktrees share the
# GCS sccache cache through path normalization.
#
# Usage:
#   ./build.sh                              # cargo build --release --workspace
#   ./build.sh -- check                     # cargo check
#   ./build.sh --auto-spin -- test ...      # auto-create VM if missing
#   SPUR_BUILD_JOBS=22 ./build.sh -- build  # override default -j 8
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[build] $*" >&2; }

AUTO_SPIN=0
if [[ "${1:-}" == "--auto-spin" ]]; then
    AUTO_SPIN=1; shift
fi
if [[ "${1:-}" == "--" ]]; then shift; fi
CARGO_ARGS="${CARGO_ARGS:-${*:-build --release --workspace}}"

# ---- worktree detection ----------------------------------------------------
# Resolve toplevel from the *current* directory so workers invoking us from
# .spur/worktrees/<uuid>/ pick up that worktree's path.
GIT_TOPLEVEL=$(git rev-parse --show-toplevel 2>/dev/null || echo "")
if [[ -z "$GIT_TOPLEVEL" ]]; then
    log "Not inside a git repo. Aborting."
    exit 2
fi

if [[ "$GIT_TOPLEVEL" == *"/.spur/worktrees/"* ]]; then
    WORKTREE_UUID=$(basename "$GIT_TOPLEVEL")
    WORKTREE_KEY="worktrees/$WORKTREE_UUID"
else
    WORKTREE_KEY="main"
fi
REMOTE_DIR="spur/$WORKTREE_KEY"                       # e.g. spur/worktrees/UUID
REMOTE_ABS="\$HOME/$REMOTE_DIR"                       # expanded on the VM
REMOTE_TARGET="/mnt/cargo/targets/$WORKTREE_KEY"
JOBS="${SPUR_BUILD_JOBS:-8}"

log "Worktree: $WORKTREE_KEY  (local=$GIT_TOPLEVEL)"
log "Remote:   ~/$REMOTE_DIR   target=$REMOTE_TARGET   -j$JOBS"

# ---- ensure VM is up -------------------------------------------------------
# Distinguish three states:
#   RUNNING                       — proceed
#   TERMINATED / STOPPED / etc.   — `instances start` (cheap, ~30s)
#   MISSING                       — `spin.sh` (creates from scratch)
VM_STATUS=$(gcloud compute instances describe "$VM_NAME" \
    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
    --format='value(status)' 2>/dev/null || echo "MISSING")

wait_for_ssh() {
    log "Waiting for SSH on $VM_NAME..."
    for _ in $(seq 1 30); do
        if gcloud compute ssh "$VM_NAME" \
                --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
                --tunnel-through-iap --quiet \
                --command='true' >/dev/null 2>&1; then
            return 0
        fi
        sleep 5
    done
    return 1
}

case "$VM_STATUS" in
    RUNNING)
        ;;
    TERMINATED|STOPPED|STOPPING|SUSPENDED|SUSPENDING)
        if [[ $AUTO_SPIN -eq 1 ]]; then
            log "VM $VM_NAME is $VM_STATUS — starting..."
            gcloud compute instances start "$VM_NAME" \
                --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
                --quiet || { log "start failed"; exit 1; }
            wait_for_ssh || { log "SSH never came up"; exit 1; }
        else
            log "VM $VM_NAME is $VM_STATUS. Pass --auto-spin to start it."
            exit 1
        fi
        ;;
    MISSING)
        if [[ $AUTO_SPIN -eq 1 ]]; then
            log "VM $VM_NAME missing — spinning a fresh one..."
            "$SCRIPT_DIR/spin.sh" || { log "spin.sh failed"; exit 1; }
        else
            log "VM $VM_NAME missing. Pass --auto-spin to create it."
            exit 1
        fi
        ;;
    *)
        log "VM $VM_NAME in unexpected state: $VM_STATUS"
        exit 1
        ;;
esac

TRANSPORT="$SCRIPT_DIR/_gcloud-ssh.sh"
export GCP_PROJECT GCP_ZONE

# ---- enumerate tracked files for this worktree -----------------------------
log "Enumerating git-tracked files..."
cd "$GIT_TOPLEVEL"
FILE_LIST=$(mktemp)
trap 'rm -f "$FILE_LIST"' EXIT
git ls-files -z --cached --others --exclude-standard >"$FILE_LIST"
COUNT=$(tr -cd '\0' <"$FILE_LIST" | wc -c | tr -d ' ')
log "  $COUNT files"

# Ensure remote parent dir exists (idempotent).
gcloud compute ssh "$VM_NAME" \
    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
    --tunnel-through-iap --quiet \
    --command="mkdir -p ~/$REMOTE_DIR $REMOTE_TARGET /mnt/cargo/cargo-home /mnt/cargo/rustup" >/dev/null

log "Syncing to $VM_NAME:~/$REMOTE_DIR ..."
rsync -az --delete -0 --files-from="$FILE_LIST" \
    -e "$TRANSPORT" \
    "$GIT_TOPLEVEL/" "$VM_NAME:$REMOTE_DIR/"

# ---- run cargo on the VM ---------------------------------------------------
log "Running: cargo $CARGO_ARGS  -j$JOBS"
gcloud compute ssh "$VM_NAME" \
    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
    --tunnel-through-iap --quiet \
    --command="bash -lc '
        set -e
        cd ~/$REMOTE_DIR
        # Strip any legacy CARGO_TARGET_DIR from profile.d.
        source /etc/profile.d/spur-build.sh 2>/dev/null || true
        # Worktree-specific overrides (must come after profile.d).
        export CARGO_TARGET_DIR=$REMOTE_TARGET
        export SCCACHE_BASEDIRS=$REMOTE_ABS
        export CARGO_BUILD_JOBS=$JOBS
        sccache --start-server >/dev/null 2>&1 || true
        cargo $CARGO_ARGS
        echo
        echo \"--- sccache stats ($WORKTREE_KEY) ---\"
        sccache --show-stats | head -20
    '"

log "Done. target lives at $VM_NAME:$REMOTE_TARGET — use scripts/gcp-build/fetch.sh to pull artifacts."
