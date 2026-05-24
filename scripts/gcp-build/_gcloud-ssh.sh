#!/usr/bin/env bash
# Internal rsync transport. Invoked by rsync as:
#   _gcloud-ssh.sh <host> <rsync-server-command...>
# We forward to `gcloud compute ssh` so OS Login / IAP / SSH key management
# all go through gcloud's normal auth flow.
set -euo pipefail

: "${GCP_PROJECT:=wiilearn}"
: "${GCP_ZONE:=asia-southeast1-a}"
# Force numpy-enabled python so rsync over IAP isn't capped at ~3.5 KB/s.
# SITEPACKAGES=1 is required: gcloud runs python with -S by default, which
# hides Homebrew's site-packages where numpy lives.
: "${CLOUDSDK_PYTHON:=/opt/homebrew/bin/python3.13}"
: "${CLOUDSDK_PYTHON_SITEPACKAGES:=1}"
export CLOUDSDK_PYTHON CLOUDSDK_PYTHON_SITEPACKAGES

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
