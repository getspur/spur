#!/usr/bin/env bash
# One-time setup: GCS bucket, persistent cache disk, build service account.
# Idempotent — re-running is safe.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[init] $*" >&2; }

gcloud config set project "$GCP_PROJECT" >/dev/null

log "Enabling required APIs..."
gcloud services enable \
    compute.googleapis.com \
    storage.googleapis.com \
    iam.googleapis.com

log "Ensuring GCS sccache bucket: gs://$SCCACHE_BUCKET"
if ! gcloud storage buckets describe "gs://$SCCACHE_BUCKET" >/dev/null 2>&1; then
    gcloud storage buckets create "gs://$SCCACHE_BUCKET" \
        --location="$GCP_REGION" \
        --uniform-bucket-level-access \
        --default-storage-class=STANDARD
else
    log "  exists"
fi

log "Setting 30-day lifecycle on sccache bucket..."
LIFECYCLE_JSON=$(mktemp)
cat >"$LIFECYCLE_JSON" <<'JSON'
{
  "lifecycle": {
    "rule": [
      { "action": {"type": "Delete"}, "condition": {"age": 30} }
    ]
  }
}
JSON
gcloud storage buckets update "gs://$SCCACHE_BUCKET" --lifecycle-file="$LIFECYCLE_JSON"
rm -f "$LIFECYCLE_JSON"

log "Ensuring service account: $BUILD_SA_EMAIL"
if ! gcloud iam service-accounts describe "$BUILD_SA_EMAIL" >/dev/null 2>&1; then
    gcloud iam service-accounts create "$BUILD_SA_NAME" \
        --display-name="SPUR remote builder"
else
    log "  exists"
fi

log "Granting bucket read/write to service account..."
gcloud storage buckets add-iam-policy-binding "gs://$SCCACHE_BUCKET" \
    --member="serviceAccount:$BUILD_SA_EMAIL" \
    --role="roles/storage.objectUser" >/dev/null

log "Ensuring cache disk: $CACHE_DISK ($CACHE_DISK_SIZE_GB GB, $CACHE_DISK_TYPE) in $GCP_ZONE"
if ! gcloud compute disks describe "$CACHE_DISK" --zone="$GCP_ZONE" >/dev/null 2>&1; then
    gcloud compute disks create "$CACHE_DISK" \
        --zone="$GCP_ZONE" \
        --size="${CACHE_DISK_SIZE_GB}GB" \
        --type="$CACHE_DISK_TYPE"
else
    log "  exists"
fi

log "Done. Next: ./spin.sh"
