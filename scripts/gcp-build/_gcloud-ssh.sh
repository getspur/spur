#!/usr/bin/env bash
# Internal rsync transport. Invoked by rsync as:
#   _gcloud-ssh.sh <host> <rsync-server-command...>
# We forward to `gcloud compute ssh` so OS Login / IAP / SSH key management
# all go through gcloud's normal auth flow.
set -euo pipefail

: "${GCP_PROJECT:=wiilearn}"
: "${GCP_ZONE:=us-central1-a}"

host="$1"; shift

# Strip an optional `user@` prefix — gcloud handles username via OS Login.
host="${host##*@}"

exec gcloud compute ssh \
    --project="$GCP_PROJECT" \
    --zone="$GCP_ZONE" \
    --tunnel-through-iap \
    --quiet \
    "$host" \
    -- "$@"
