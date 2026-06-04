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

# Node.js LTS (system-wide). Needed for production spur-notebook builds where
# Tauri embeds the Vite-built frontend. Corepack is enabled now so a future
# pnpm switch does not require re-provisioning the VM.
if ! command -v node >/dev/null 2>&1; then
    echo "Installing Node.js LTS..."
    curl -fsSL https://deb.nodesource.com/setup_lts.x | bash -
    apt-get install -y --no-install-recommends nodejs
fi
corepack enable

# sccache (system-wide). Pinned to 0.15.0 because it's the first version that
# treats --remap-path-prefix as cacheable (#2270) and excludes
# CARGO_ENCODED_RUSTFLAGS from the env hash (#2651) — both required for
# cross-worktree Rust cache hits via the path-prefix remap in build.sh.
SCCACHE_VERSION=v0.15.0
INSTALLED=$(/usr/local/bin/sccache --version 2>/dev/null | awk '{print "v"$2}' || echo "")
if [[ "$INSTALLED" != "$SCCACHE_VERSION" ]]; then
    curl -fsSL "https://github.com/mozilla/sccache/releases/download/${SCCACHE_VERSION}/sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        | tar xz -C /tmp
    install -m 0755 "/tmp/sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache
fi

# rustup + stable toolchain on the cache disk. Survives across boots; only
# installs on a fresh disk (i.e. after `spin.sh` provisions a new pd or after
# a disk swap). Workers' `rust-toolchain.toml` pins fetch additional toolchains
# lazily, so we only bootstrap stable here.
if [[ ! -x "$CACHE_MNT/cargo-home/bin/rustup" ]]; then
    echo "Installing rustup + stable toolchain..."
    BUILD_USER=$(stat -c %U "$CACHE_MNT") # the user owning /mnt/cargo
    if [[ -z "$BUILD_USER" || "$BUILD_USER" == "root" ]]; then
        # Fresh disk: chmod 1777 is set below, but we still need /mnt/cargo/cargo-home
        # owned by the same user that runs cargo. Pick the first OS Login user
        # (their home was created on first SSH); fall back to current SSH user.
        BUILD_USER=$(ls /home 2>/dev/null | head -1 || echo root)
    fi
    sudo -u "$BUILD_USER" -H sh -c \
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
         RUSTUP_HOME=$CACHE_MNT/rustup CARGO_HOME=$CACHE_MNT/cargo-home \
         sh -s -- -y --default-toolchain stable --no-modify-path --profile minimal"
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

# Deno (system-wide). The spur-notebook Deno Jupyter kernel provisions a
# kernelspec whose argv is `deno jupyter --kernel --conn {connection_file}`
# (crates/spur-notebook/.../kernel_provision.rs). Without `deno` on PATH the
# kernel-provisioning + deno-kernel integration tests silently skip on the VM
# (deno_binary_for_test() returns None). Pinned for reproducible reprovisions;
# installed to /usr/local/bin so every OS Login user — and find_binary_on_path —
# resolves it.
DENO_VERSION=v2.8.1
INSTALLED_DENO=$(deno --version 2>/dev/null | awk 'NR==1 {print "v"$2}' || echo "")
if [[ "$INSTALLED_DENO" != "$DENO_VERSION" ]]; then
    echo "Installing Deno ${DENO_VERSION}..."
    command -v unzip >/dev/null 2>&1 || apt-get install -y --no-install-recommends unzip
    curl -fsSL "https://github.com/denoland/deno/releases/download/${DENO_VERSION}/deno-x86_64-unknown-linux-gnu.zip" \
        -o /tmp/deno.zip
    unzip -o /tmp/deno.zip -d /tmp
    install -m 0755 /tmp/deno /usr/local/bin/deno
    rm -f /tmp/deno.zip /tmp/deno
fi

# Drop a profile.d snippet so every login shell sees the right env.
# Note: CARGO_TARGET_DIR and SCCACHE_BASEDIRS are NOT set here — build.sh sets
# them per-invocation so each worktree gets an isolated target/ and so sccache
# basedir-normalizes paths across worktrees.
# Per-invocation rustc wrapper: sets SCCACHE_BASEDIRS to the workspace root
# (walking up from $PWD until the topmost Cargo.toml). sccache strips this
# prefix before hashing rustc inputs so identical source content at different
# worktree paths produces the same cache key.
cat >/usr/local/bin/sccache-worktree <<'WRAPPER'
#!/bin/bash
ROOT=""
DIR="$PWD"
while [[ "$DIR" != "/" ]]; do
    if [[ -f "$DIR/Cargo.toml" ]]; then
        ROOT="$DIR"
    fi
    DIR="$(dirname "$DIR")"
done
[[ -n "$ROOT" ]] && export SCCACHE_BASEDIRS="$ROOT"
exec /usr/local/bin/sccache "$@"
WRAPPER
chmod 0755 /usr/local/bin/sccache-worktree

# C/C++ wrappers for build-script-driven compiles (libduckdb-sys, ring,
# tree-sitter-*, zstd-sys, ...). $PWD at invocation is cargo's project root,
# not OUT_DIR, so paths embedded in __FILE__/preprocessed output stay
# worktree-absolute and miss the cache cross-worktree. Setting
# SCCACHE_BASEDIR=$OUT_DIR — inherited from build.rs — normalizes those
# paths to relative form. Verified: libduckdb-sys cold compile drops from
# ~280s to ~107s cross-worktree at 99% C++ hit rate.
cat >/usr/local/bin/sccache-cc <<'WRAPPER'
#!/bin/bash
[[ -n "$OUT_DIR" ]] && export SCCACHE_BASEDIR="$OUT_DIR" || export SCCACHE_BASEDIR="$PWD"
exec /usr/local/bin/sccache /usr/bin/cc "$@"
WRAPPER
cat >/usr/local/bin/sccache-cxx <<'WRAPPER'
#!/bin/bash
[[ -n "$OUT_DIR" ]] && export SCCACHE_BASEDIR="$OUT_DIR" || export SCCACHE_BASEDIR="$PWD"
exec /usr/local/bin/sccache /usr/bin/c++ "$@"
WRAPPER
chmod 0755 /usr/local/bin/sccache-cc /usr/local/bin/sccache-cxx

cat >/etc/profile.d/spur-build.sh <<EOF
export CARGO_HOME=$CACHE_MNT/cargo-home
export RUSTUP_HOME=$CACHE_MNT/rustup
export RUSTC_WRAPPER=/usr/local/bin/sccache-worktree
# Wrap cc/c++ too so build.rs-driven C/C++ compiles (libduckdb-sys is the
# big one — ~280s of single-threaded amalgamation otherwise) hit the same
# GCS cache. The cc crate honors CC/CXX env vars and dispatches per-file
# in parallel via cargo's jobserver, so every TU is independently cacheable.
export CC=/usr/local/bin/sccache-cc
export CXX=/usr/local/bin/sccache-cxx
export SCCACHE_GCS_BUCKET=${SCCACHE_GCS_BUCKET}
export SCCACHE_GCS_RW_MODE=READ_WRITE
export SCCACHE_GCS_KEY_PATH=
# Disable incremental compilation: sccache marks incremental rustc invocations
# non-cacheable, which is why Rust cache hit rate sat at 0% (vs 97% for C/C++).
# Non-incremental builds are slower locally but trade off against shared GCS hits.
export CARGO_INCREMENTAL=0
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
# Disk-pressure watchdog: reclaim space whenever /mnt/cargo crosses the strict
# high-water mark. Independent of the idle-shutdown check above — fires even
# when other workers are actively building. Per-delegation worktree target
# dirs balloon to 40-110 GB each and three concurrent workers can fill the
# disk in well under an hour.
#
# Reclaim is TIERED and re-checks disk between tiers, stopping as soon as usage
# drops back under HIGH_WATER. Every tier is idle-guarded (mtime within
# IDLE_MIN ⇒ a build is still touching it ⇒ skip) so we never delete files an
# in-flight compile depends on:
#   1. idle per-delegation worktree target dirs   (cheap, never reused)
#   2. stale build temp — /mnt/cargo/tmp (the big TMPDIR set by build.sh) and
#      /tmp (small sync manifests on the boot disk)
#   3. LAST RESORT: idle targets/main — the shared main-repo cache. Clearing it
#      forces a cold main rebuild, so it only fires if 1+2 left us over the mark.
# ---------------------------------------------------------------------------
cat >/usr/local/sbin/spur-disk-watchdog <<'SCRIPT'
#!/bin/bash
set -u
HIGH_WATER=70       # percent — strict high-water mark
IDLE_MIN=10         # only reclaim space idle this long (no active build touching it)
WORKTREES=/mnt/cargo/targets/worktrees
MAIN_TARGET=/mnt/cargo/targets/main
TMP_DIRS=(/mnt/cargo/tmp /tmp)

used_pct() { df --output=pcent /mnt/cargo | tail -1 | tr -dc 0-9; }
over_mark() { local u; u=$(used_pct); [[ -n "$u" && "$u" -ge "$HIGH_WATER" ]]; }

over_mark || exit 0
echo "disk at $(used_pct)% (>= ${HIGH_WATER}%) — reclaiming space"

# --- Tier 1: idle per-delegation worktree target dirs ---
if [[ -d "$WORKTREES" ]]; then
    for dir in "$WORKTREES"/*/; do
        [[ -d "$dir" ]] || continue
        if find "$dir" -mmin -"$IDLE_MIN" -type f -print -quit | grep -q .; then
            echo "  keep worktree $(basename "$dir") (active)"
            continue
        fi
        SIZE=$(du -sh "$dir" 2>/dev/null | cut -f1)
        rm -rf "$dir"
        echo "  pruned worktree $(basename "$dir") ($SIZE)"
    done
fi

# --- Tier 2: stale build temp (only when still over the mark) ---
if over_mark; then
    echo "  still at $(used_pct)% — sweeping stale build temp (>${IDLE_MIN}m)"
    for t in "${TMP_DIRS[@]}"; do
        [[ -d "$t" ]] || continue
        # -mmin +IDLE_MIN keeps anything an in-flight rustc/linker still touches.
        # Stay on the same filesystem (-xdev); drop stale files then empty dirs.
        find "$t" -mindepth 1 -xdev -mmin +"$IDLE_MIN" -type f -delete 2>/dev/null || true
        find "$t" -mindepth 1 -xdev -mmin +"$IDLE_MIN" -type d -empty -delete 2>/dev/null || true
        echo "  swept $t"
    done
fi

# --- Tier 3 (LAST RESORT): idle shared main-repo cache ---
if over_mark && [[ -d "$MAIN_TARGET" ]]; then
    if find "$MAIN_TARGET" -mmin -"$IDLE_MIN" -type f -print -quit | grep -q .; then
        echo "  still at $(used_pct)% but targets/main is active — leaving it"
    else
        SIZE=$(du -sh "$MAIN_TARGET" 2>/dev/null | cut -f1)
        echo "  still at $(used_pct)% — clearing idle targets/main ($SIZE, forces cold main rebuild)"
        rm -rf "${MAIN_TARGET:?}"/*
    fi
fi

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
