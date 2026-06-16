#!/usr/bin/env bash
# Quick status: VM and durable sccache bucket.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

echo "== Project: $GCP_PROJECT  Zone: $GCP_ZONE =="
echo
echo "-- VM --"
gcloud compute instances list --project="$GCP_PROJECT" --filter="name=$VM_NAME" \
    --format='table(name,zone.basename(),status,machineType.basename(),scheduling.provisioningModel)' 2>/dev/null || true
echo
echo "-- sccache bucket --"
gcloud storage buckets describe "gs://$SCCACHE_BUCKET" --format='value(name,location,storageClass)' 2>/dev/null || echo "(not created)"
gcloud storage du -s "gs://$SCCACHE_BUCKET" 2>/dev/null || true
