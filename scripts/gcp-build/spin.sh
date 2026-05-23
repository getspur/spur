#!/usr/bin/env bash
# Create the spot build VM with the cache disk attached.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[spin] $*" >&2; }

VM_STATUS=$(gcloud compute instances describe "$VM_NAME" \
    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
    --format='value(status)' 2>/dev/null || echo "MISSING")

case "$VM_STATUS" in
    RUNNING)
        log "VM $VM_NAME already RUNNING in $GCP_ZONE. Nothing to do."
        exit 0
        ;;
    TERMINATED|STOPPED|SUSPENDED|STOPPING|SUSPENDING)
        log "VM $VM_NAME is $VM_STATUS — starting (cache disk stays attached)..."
        gcloud compute instances start "$VM_NAME" \
            --project="$GCP_PROJECT" --zone="$GCP_ZONE" --quiet
        log "Waiting for SSH..."
        for _ in $(seq 1 30); do
            if gcloud compute ssh "$VM_NAME" \
                    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
                    --tunnel-through-iap --quiet --command='true' >/dev/null 2>&1; then
                log "VM ready."
                exit 0
            fi
            sleep 5
        done
        log "SSH never came up after start"; exit 1
        ;;
    MISSING)
        : ;;  # fall through to fresh create
    *)
        log "VM $VM_NAME in unexpected state: $VM_STATUS"
        exit 1
        ;;
esac

log "Creating spot VM $VM_NAME ($VM_MACHINE_TYPE) in $GCP_ZONE..."
gcloud compute instances create "$VM_NAME" \
    --zone="$GCP_ZONE" \
    --machine-type="$VM_MACHINE_TYPE" \
    --provisioning-model=SPOT \
    --instance-termination-action=DELETE \
    --image-family="$VM_IMAGE_FAMILY" \
    --image-project="$VM_IMAGE_PROJECT" \
    --boot-disk-size="${VM_BOOT_DISK_SIZE_GB}GB" \
    --boot-disk-type=pd-balanced \
    --disk="name=$CACHE_DISK,device-name=cargo-cache,mode=rw,boot=no,auto-delete=no" \
    --service-account="$BUILD_SA_EMAIL" \
    --scopes=cloud-platform \
    --metadata="sccache-bucket=$SCCACHE_BUCKET,enable-oslogin=TRUE" \
    --metadata-from-file="startup-script=$SCRIPT_DIR/startup.sh"

log "Waiting for SSH..."
for _ in $(seq 1 30); do
    if gcloud compute ssh "$VM_NAME" --zone="$GCP_ZONE" --tunnel-through-iap \
            --command='echo ready' >/dev/null 2>&1; then
        log "SSH ready."
        break
    fi
    sleep 5
done

log "Waiting for startup-script to finish (rustup + sccache install)..."
gcloud compute ssh "$VM_NAME" --zone="$GCP_ZONE" --tunnel-through-iap --command='
    set -e
    for i in $(seq 1 120); do
        if grep -q "startup done" /var/log/spur-startup.log 2>/dev/null; then
            echo "startup-script complete"; exit 0
        fi
        sleep 5
    done
    echo "startup-script did not finish in 10 min — check /var/log/spur-startup.log" >&2
    exit 1
'

log "Installing rustup as the SSH user (one-time per VM)..."
gcloud compute ssh "$VM_NAME" --zone="$GCP_ZONE" --tunnel-through-iap --command='
    set -e
    source /etc/profile.d/spur-build.sh
    if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --default-toolchain stable --profile minimal
    fi
    rustup show
    sccache --version
'

log "VM ready. Run ./build.sh to sync sources and build."
