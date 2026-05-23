#!/usr/bin/env bash
# Delete the build VM. Cache disk and GCS bucket are preserved.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

if ! gcloud compute instances describe "$VM_NAME" --zone="$GCP_ZONE" >/dev/null 2>&1; then
    echo "[teardown] VM $VM_NAME not present in $GCP_ZONE — nothing to do."
    exit 0
fi

echo "[teardown] Deleting VM $VM_NAME (cache disk $CACHE_DISK is preserved)..."
gcloud compute instances delete "$VM_NAME" \
    --zone="$GCP_ZONE" \
    --quiet \
    --keep-disks=data

echo "[teardown] Done."
