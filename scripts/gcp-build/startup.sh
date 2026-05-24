#!/usr/bin/env bash
# VM startup script. Runs as root on every boot.
# - Mounts persistent cache disk
# - Installs build deps, rustup, sccache
# - Writes per-user cargo config pointing target/ to cache disk + sccache->GCS
set -euo pipefail

LOG=/var/log/spur-startup.log
exec >>"$LOG" 2>&1
echo "=== spur-startup $(date -u +%FT%TZ) ==="

CACHE_DEV=/dev/disk/by-id/google-cargo-cache
CACHE_MNT=/mnt/cargo

# Format on first attach only.
if ! blkid "$CACHE_DEV" >/dev/null 2>&1; then
    echo "Formatting fresh cache disk..."
    mkfs.ext4 -F -L cargo-cache "$CACHE_DEV"
fi
mkdir -p "$CACHE_MNT"
mountpoint -q "$CACHE_MNT" || mount "$CACHE_DEV" "$CACHE_MNT"

# Default OS Login user is created on first ssh; chown lazily there.

# Pull bucket name from instance metadata.
SCCACHE_GCS_BUCKET=$(curl -fsS -H "Metadata-Flavor: Google" \
    http://metadata.google.internal/computeMetadata/v1/instance/attributes/sccache-bucket 2>/dev/null || echo "")

# System deps.
export DEBIAN_FRONTEND=noninteractive
if ! command -v cc >/dev/null 2>&1; then
    apt-get update
    apt-get install -y --no-install-recommends \
        build-essential pkg-config libssl-dev cmake git curl ca-certificates \
        protobuf-compiler clang lld rsync \
        libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev \
        libayatana-appindicator3-dev librsvg2-dev libjavascriptcoregtk-4.1-dev
fi

# sccache (system-wide).
SCCACHE_VERSION=v0.8.2
if ! command -v sccache >/dev/null 2>&1; then
    curl -fsSL "https://github.com/mozilla/sccache/releases/download/${SCCACHE_VERSION}/sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        | tar xz -C /tmp
    install -m 0755 "/tmp/sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache
fi

# DuckDB CLI (system-wide). Needed by analyst tests that shell out to `duckdb`;
# without it those tests silently skip on the build VM.
DUCKDB_VERSION=v1.5.3
if ! command -v duckdb >/dev/null 2>&1; then
    curl -fsSL "https://github.com/duckdb/duckdb/releases/download/${DUCKDB_VERSION}/duckdb_cli-linux-amd64.zip" \
        -o /tmp/duckdb_cli.zip
    apt-get install -y --no-install-recommends unzip
    unzip -o /tmp/duckdb_cli.zip -d /tmp
    install -m 0755 /tmp/duckdb /usr/local/bin/duckdb
    rm -f /tmp/duckdb_cli.zip /tmp/duckdb
fi

# Drop a profile.d snippet so every login shell sees the right env.
# Note: CARGO_TARGET_DIR and SCCACHE_BASEDIRS are NOT set here — build.sh sets
# them per-invocation so each worktree gets an isolated target/ and so sccache
# basedir-normalizes paths across worktrees.
cat >/etc/profile.d/spur-build.sh <<EOF
export CARGO_HOME=$CACHE_MNT/cargo-home
export RUSTUP_HOME=$CACHE_MNT/rustup
export RUSTC_WRAPPER=/usr/local/bin/sccache
export SCCACHE_GCS_BUCKET=${SCCACHE_GCS_BUCKET}
export SCCACHE_GCS_RW_MODE=READ_WRITE
export SCCACHE_GCS_KEY_PATH=
export PATH="\$CARGO_HOME/bin:\$PATH"
EOF
chmod 0644 /etc/profile.d/spur-build.sh

# Permissive ownership on cache (multi-user OS Login).
chmod 1777 "$CACHE_MNT"

# ---------------------------------------------------------------------------
# Idle auto-shutdown: terminate the VM after 15 min with no build / ssh / target
# activity. Spot VMs created with --instance-termination-action=DELETE convert
# `shutdown -h` into instance deletion; the persistent cache disk survives.
# Override the threshold by passing instance metadata `idle-shutdown-minutes`.
# ---------------------------------------------------------------------------
IDLE_MINUTES=$(curl -fsS -H "Metadata-Flavor: Google" \
    http://metadata.google.internal/computeMetadata/v1/instance/attributes/idle-shutdown-minutes 2>/dev/null || echo "")
[[ -z "$IDLE_MINUTES" || ! "$IDLE_MINUTES" =~ ^[0-9]+$ ]] && IDLE_MINUTES=15

# Heredoc is single-quoted so $-references stay literal inside the script.
# IDLE_MIN substitution is done by sed afterward.
cat >/usr/local/sbin/spur-autoshutdown <<'SCRIPT'
#!/bin/bash
# Returns 0 if VM is idle (should shut down), 1 if active.
set -u
IDLE_MIN=__IDLE_MIN__
TARGETS=/mnt/cargo/targets

# 1. Any cargo or rustc process? -> active. sccache runs as a long-lived
#    daemon and is excluded; it would otherwise pin the VM up forever.
if pgrep -x cargo >/dev/null || pgrep -x rustc >/dev/null; then
    echo "active: build process running"
    exit 1
fi

# 2. Any logged-in user (ssh session)? -> active.
if [[ $(who | wc -l) -gt 0 ]]; then
    echo "active: ssh session(s) open"
    exit 1
fi

# 3. Any file under targets/ modified within IDLE_MIN minutes? -> active.
if [[ -d "$TARGETS" ]] && find "$TARGETS" -mmin -"$IDLE_MIN" -type f -print -quit | grep -q .; then
    echo "active: target/ modified in last $IDLE_MIN min"
    exit 1
fi

echo "idle for $IDLE_MIN+ min — shutting down"

# Pre-shutdown cleanup: per-delegation worktree target dirs are not reused
# across reboots and were observed filling /mnt/cargo to 99%, causing ld
# bus errors on the next boot. The shared targets/main cache is preserved.
WORKTREES=/mnt/cargo/targets/worktrees
if [[ -d "$WORKTREES" ]]; then
    BEFORE=$(df -h /mnt/cargo | awk 'NR==2 {print $3"/"$2" ("$5")"}')
    rm -rf "$WORKTREES"/*
    AFTER=$(df -h /mnt/cargo | awk 'NR==2 {print $3"/"$2" ("$5")"}')
    echo "pruned $WORKTREES: $BEFORE -> $AFTER"
fi

/sbin/shutdown -h now "SPUR autoshutdown: idle $IDLE_MIN+ min"
exit 0
SCRIPT
sed -i "s/__IDLE_MIN__/$IDLE_MINUTES/" /usr/local/sbin/spur-autoshutdown
chmod 0755 /usr/local/sbin/spur-autoshutdown

cat >/etc/systemd/system/spur-autoshutdown.service <<'EOF'
[Unit]
Description=SPUR builder idle-shutdown check
[Service]
Type=oneshot
ExecStart=/usr/local/sbin/spur-autoshutdown
StandardOutput=journal
EOF

cat >/etc/systemd/system/spur-autoshutdown.timer <<EOF
[Unit]
Description=Run SPUR idle-shutdown check every 5 minutes
[Timer]
# First check after boot grace period so initial setup doesn't trigger it.
OnBootSec=${IDLE_MINUTES}min
OnUnitActiveSec=5min
AccuracySec=30s
Unit=spur-autoshutdown.service
[Install]
WantedBy=timers.target
EOF

systemctl daemon-reload
systemctl enable --now spur-autoshutdown.timer

# ---------------------------------------------------------------------------
# Disk-pressure watchdog: prune idle worktree target dirs whenever
# /mnt/cargo crosses the high-water mark. Independent of the idle-shutdown
# check above — fires even when other workers are actively building.
# Per-delegation worktree target dirs balloon to 40-110 GB each and three
# concurrent workers can fill a 400 GB disk in well under an hour.
# ---------------------------------------------------------------------------
cat >/usr/local/sbin/spur-disk-watchdog <<'SCRIPT'
#!/bin/bash
set -u
HIGH_WATER=85       # percent
IDLE_MIN=10         # only prune worktrees idle this long
WORKTREES=/mnt/cargo/targets/worktrees

USED=$(df --output=pcent /mnt/cargo | tail -1 | tr -dc 0-9)
[[ -z "$USED" || "$USED" -lt "$HIGH_WATER" ]] && exit 0

echo "disk at ${USED}% — pruning idle worktrees (>${IDLE_MIN}m)"
[[ ! -d "$WORKTREES" ]] && exit 0

for dir in "$WORKTREES"/*/; do
    [[ -d "$dir" ]] || continue
    # Skip if any file modified within IDLE_MIN minutes — build still active.
    if find "$dir" -mmin -"$IDLE_MIN" -type f -print -quit | grep -q .; then
        echo "  keep $(basename "$dir") (active)"
        continue
    fi
    SIZE=$(du -sh "$dir" 2>/dev/null | cut -f1)
    rm -rf "$dir"
    echo "  pruned $(basename "$dir") ($SIZE)"
done

df -h /mnt/cargo | tail -1
SCRIPT
chmod 0755 /usr/local/sbin/spur-disk-watchdog

cat >/etc/systemd/system/spur-disk-watchdog.service <<'EOF'
[Unit]
Description=SPUR builder disk-pressure watchdog
[Service]
Type=oneshot
ExecStart=/usr/local/sbin/spur-disk-watchdog
StandardOutput=journal
EOF

cat >/etc/systemd/system/spur-disk-watchdog.timer <<'EOF'
[Unit]
Description=Run SPUR disk-pressure watchdog every 2 minutes
[Timer]
OnBootSec=2min
OnUnitActiveSec=2min
AccuracySec=15s
Unit=spur-disk-watchdog.service
[Install]
WantedBy=timers.target
EOF

systemctl daemon-reload
systemctl enable --now spur-disk-watchdog.timer

echo "=== startup done $(date -u +%FT%TZ) ==="
