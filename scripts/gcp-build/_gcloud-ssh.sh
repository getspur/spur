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

# Transport mode is selected by build.sh and handed down via SPUR_SSH_IAP_FLAG:
#   ""                     -> direct SSH to the VM's external IP (skip IAP)
#   "--tunnel-through-iap" -> route through the IAP tunnel
# Use `-` (not `:-`) so an explicitly-empty value means "direct"; only an
# UNSET var (standalone invocation) defaults to the safe IAP path.
IAP_FLAG="${SPUR_SSH_IAP_FLAG---tunnel-through-iap}"

if [[ -z "$IAP_FLAG" ]]; then
    DIRECT_SSH_PORT="${SPUR_DIRECT_SSH_PORT:-22}"
    if [[ ! "$DIRECT_SSH_PORT" =~ ^[0-9]+$ ]]; then
        echo "Invalid SPUR_DIRECT_SSH_PORT=$DIRECT_SSH_PORT" >&2
        exit 2
    fi
    if [[ "$DIRECT_SSH_PORT" != "22" ]]; then
        exec gcloud compute ssh \
            --project="$GCP_PROJECT" \
            --zone="$GCP_ZONE" \
            --ssh-flag="-p $DIRECT_SSH_PORT" \
            --quiet \
            "$host" \
            -- "$@"
    fi
fi

# $IAP_FLAG is intentionally unquoted: empty -> no arg (direct), else one flag.
exec gcloud compute ssh \
    --project="$GCP_PROJECT" \
    --zone="$GCP_ZONE" \
    $IAP_FLAG \
    --quiet \
    "$host" \
    -- "$@"
