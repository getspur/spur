#!/usr/bin/env bash
# Provision the cloud-build VM for macOS (aarch64-apple-darwin) cross-compiles
# driven by `scripts/spur-cargo zigbuild`.
#
# What it does:
#   1. Copies a minimal macOS "linker SDK" from THIS Mac to the VM: every
#      .tbd link stub (Frameworks + PrivateFrameworks + usr/lib, symlinks
#      materialized with -L so nothing dangles), usr/include headers, and
#      libclang_rt.osx.a (Apple compiler-rt; pyke's prebuilt onnxruntime
#      links it explicitly). Total ~70 MB.
#   2. On the VM: adds the aarch64-apple-darwin rust-std to EVERY installed
#      toolchain (the repo pins 1.94.1 via rust-toolchain.toml — adding only
#      to `stable` leaves the pinned toolchain without a darwin std and the
#      build dies with E0463), installs zig (latest stable < 0.16) and
#      cargo-zigbuild + bindgen-cli, and appends SDKROOT / darwin-scoped
#      CFLAGS to /etc/profile.d/spur-build.sh.
#   3. Pre-generates libproc's darwin bindings against the synced SDK and
#      installs a VM-side helper to plant them into libproc's OUT_DIR —
#      libproc 0.14's build.rs is host-cfg-gated, so cross builds never
#      generate them (see the POC doc, "libproc" section).
#
# Run from a Mac with the Xcode Command Line Tools installed. Idempotent —
# safe to re-run; REQUIRED again after a spot preemption replaces the VM
# (zig/SDK/bindings live on instance-store /mnt/cargo, which does not
# survive).
#
# See: docs/superpowers/specs/2026-07-07-zigbuild-macos-cross-poc.md
set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="${SCRIPT_PATH%/*}"
[[ "$SCRIPT_DIR" == "$SCRIPT_PATH" ]] && SCRIPT_DIR="."
SCRIPT_DIR=$(cd "$SCRIPT_DIR" && pwd -P)

log() { echo "[zigbuild-provision] $*" >&2; }

# ---- resolve the cloud-build pipeline (same search order as spur-cargo) ----
resolve_cloud_build_dir() {
    local candidate git_toplevel repo_root notebook_repo
    if [[ -n "${SPUR_CLOUD_BUILD_SH:-}" ]]; then
        candidate="$(dirname "$SPUR_CLOUD_BUILD_SH")"
        [[ -d "$candidate" ]] && { printf '%s\n' "$candidate"; return 0; }
        return 1
    fi
    candidate="$SCRIPT_DIR/cloud-build"
    [[ -e "$candidate/build.sh" ]] && { printf '%s\n' "$candidate"; return 0; }
    if [[ -n "${SPUR_NOTEBOOK_REPO:-}" ]]; then
        candidate="$SPUR_NOTEBOOK_REPO/scripts/cloud-build"
        [[ -e "$candidate/build.sh" ]] && { printf '%s\n' "$candidate"; return 0; }
    fi
    git_toplevel=$(git -C "$SCRIPT_DIR/.." rev-parse --show-toplevel 2>/dev/null || true)
    [[ -n "$git_toplevel" ]] || return 1
    if [[ "$git_toplevel" == *"/.spur/worktrees/"* ]]; then
        repo_root="$(dirname "$(dirname "$(dirname "$git_toplevel")")")"
    else
        repo_root="$git_toplevel"
    fi
    notebook_repo="$(dirname "$repo_root")/spur-notebook"
    candidate="$notebook_repo/scripts/cloud-build"
    [[ -e "$candidate/build.sh" ]] && { printf '%s\n' "$candidate"; return 0; }
    return 1
}

CB="$(resolve_cloud_build_dir)" || {
    log "cannot find scripts/cloud-build (checked SPUR_CLOUD_BUILD_SH, sibling spur-notebook checkout)"
    exit 2
}
# provider_choose_transport expects SCRIPT_DIR to be the cloud-build dir.
SCRIPT_DIR="$CB"
# shellcheck disable=SC1091
source "$CB/config.env"
# shellcheck disable=SC1090
source "$CB/provider-${SPUR_CLOUD}.sh"
provider_choose_transport
log "VM: $REMOTE_HOST via $TRANSPORT_MODE"

remote_ssh() { provider_remote_ssh "$@"; }

# ---- local SDK bits ---------------------------------------------------------
SDK="$(xcrun --show-sdk-path)"
[[ -d "$SDK" ]] || { log "no macOS SDK (xcrun --show-sdk-path failed)"; exit 2; }
log "Local SDK: $SDK ($(xcrun --show-sdk-version))"

clang_rt_candidates=(/Library/Developer/CommandLineTools/usr/lib/clang/*/lib/darwin/libclang_rt.osx.a)
CLANG_RT=""
for c in "${clang_rt_candidates[@]}"; do
    [[ -f "$c" ]] && { CLANG_RT="$c"; break; }
done
[[ -n "$CLANG_RT" ]] || { log "libclang_rt.osx.a not found under CommandLineTools"; exit 2; }

REMOTE_SDK=/mnt/cargo/macsdk/MacOSX.sdk

log "Enumerating SDK .tbd link stubs..."
TBD_LIST=$(mktemp)
trap 'rm -f "$TBD_LIST"' EXIT
(
    cd "$SDK"
    find System/Library/Frameworks -name '*.tbd'
    find System/Library/PrivateFrameworks -name '*.tbd' 2>/dev/null
    find usr/lib -name '*.tbd'
    echo SDKSettings.json
) >"$TBD_LIST"
log "  $(wc -l <"$TBD_LIST" | tr -d ' ') stubs"

remote_ssh --command="mkdir -p $REMOTE_SDK/usr/include $REMOTE_SDK/usr/lib" >/dev/null

# -L materializes symlinks: framework-top X.tbd files are symlinks through
# Versions/Current (itself a symlink), and neither intermediate link is in the
# file list — copied as symlinks they dangle on the VM and zig reports
# "unable to find framework".
log "Syncing .tbd stubs (symlinks dereferenced)..."
rsync -azL --files-from="$TBD_LIST" -e "$TRANSPORT" "$SDK/" "$REMOTE_HOST:$REMOTE_SDK/"
log "Syncing usr/include headers..."
rsync -azL -e "$TRANSPORT" "$SDK/usr/include/" "$REMOTE_HOST:$REMOTE_SDK/usr/include/"
log "Syncing libclang_rt.osx.a -> \$SDKROOT/usr/lib/ ..."
rsync -az -e "$TRANSPORT" "$CLANG_RT" "$REMOTE_HOST:$REMOTE_SDK/usr/lib/libclang_rt.osx.a"

# ---- remote provisioning ----------------------------------------------------
log "Uploading remote provisioning script..."
REMOTE_PROVISION=/tmp/spur-zigbuild-provision-remote.sh
PROVISION_LOCAL=$(mktemp)
cat >"$PROVISION_LOCAL" <<'REMOTE_EOF'
#!/usr/bin/env bash
# Remote half of scripts/zigbuild-provision-vm.sh. Idempotent.
set -euo pipefail
source /etc/profile.d/spur-build.sh
mkdir -p /mnt/cargo/zig-dist /mnt/cargo/tmp /mnt/cargo/macsdk
export TMPDIR=/mnt/cargo/tmp

echo "== rustup target aarch64-apple-darwin =="
# Every toolchain: the repo pin (rust-toolchain.toml) resolves to its own
# toolchain inside ~/spur/main; adding only to the default leaves it without
# a darwin std (E0463 "can't find crate for core").
for tc in $(rustup toolchain list | awk '{print $1}'); do
    rustup target add --toolchain "$tc" aarch64-apple-darwin
done

echo "== zig =="
if [ ! -x /mnt/cargo/zig/zig ]; then
    cd /mnt/cargo/zig-dist
    curl -fsSL https://ziglang.org/download/index.json -o index.json
    url=$(python3 - <<'PY'
import json
data = json.load(open('index.json'))
best = None
for ver, info in data.items():
    if ver == 'master' or '-' in ver:
        continue
    parts = tuple(int(x) for x in ver.split('.'))
    if parts >= (0, 16, 0):
        continue  # stay on a cargo-zigbuild-proven line
    t = info.get('aarch64-linux')
    if not t:
        continue
    if best is None or parts > best[0]:
        best = (parts, t['tarball'])
print(best[1])
PY
)
    echo "zig tarball: $url"
    curl -fsSL "$url" -o zig.tar.xz
    # zig tarballs extract to a dir named like the tarball; avoid tar -tf|head,
    # which SIGPIPEs tar under pipefail.
    base=$(basename "$url")
    dir="${base%.tar.xz}"
    tar -xf zig.tar.xz
    [ -x "/mnt/cargo/zig-dist/$dir/zig" ] || { echo "unexpected zig layout: $dir"; exit 1; }
    ln -sfn "/mnt/cargo/zig-dist/$dir" /mnt/cargo/zig
fi
ln -sf /mnt/cargo/zig/zig "$CARGO_HOME/bin/zig"
echo "zig version: $(/mnt/cargo/zig/zig version)"

echo "== cargo-zigbuild =="
command -v cargo-zigbuild >/dev/null 2>&1 || cargo install cargo-zigbuild --locked
echo "cargo-zigbuild: $(cargo-zigbuild --version)"

echo "== bindgen-cli (libproc bindings) =="
command -v bindgen >/dev/null 2>&1 || cargo install bindgen-cli --locked
bindgen --version

echo "== profile.d additions =="
if sudo -n true 2>/dev/null; then
    if ! grep -q 'zigbuild macOS cross' /etc/profile.d/spur-build.sh; then
        sudo tee -a /etc/profile.d/spur-build.sh >/dev/null <<'EOF'
# --- zigbuild macOS cross ---
export PATH="/mnt/cargo/zig:$PATH"
export SDKROOT=/mnt/cargo/macsdk/MacOSX.sdk
# Target-scoped C flags beat the plain CFLAGS/-mcpu=native above in cc-rs, so
# darwin cross compiles never inherit neoverse-v2 (SVE) codegen. The explicit
# -mcpu=apple_m1 (zig cpu spelling) also sidesteps zig cc's inconsistent
# default feature mapping, which otherwise fails NEON code (blake3, zstd)
# with "always_inline function requires target feature 'altnzcv'". Every
# Apple Silicon Mac is >= M1.
export CFLAGS_aarch64_apple_darwin="-O2 -mcpu=apple_m1"
export CXXFLAGS_aarch64_apple_darwin="-O2 -mcpu=apple_m1"
EOF
        echo "profile.d updated"
    else
        echo "profile.d already updated"
    fi
else
    echo "WARNING: no passwordless sudo; set SDKROOT/CFLAGS_aarch64_apple_darwin manually"
fi

echo "== ld64.lld link driver =="
# Final links go through clang + ld64.lld, not zig's Mach-O linker: with
# libc++ linked as a system dylib (see spur-cargo's zigbuild RUSTFLAGS),
# zig's linker fails with "relocation Overflow" on prebuilt Apple-clang
# objects (pyke onnxruntime). zig remains the C/C++ compiler. A MODERN lld
# is required — the same prebuilt objects reference objc_msgSend selector
# stubs (_objc_msgSend$sel, Xcode 14+), which lld-14 does not synthesize.
if sudo -n true 2>/dev/null; then
    command -v ld64.lld-19 >/dev/null 2>&1 \
        || sudo apt-get install -y -qq lld-19 >/dev/null 2>&1 \
        || sudo apt-get install -y -qq lld-16 >/dev/null 2>&1 || true
fi
cat > /mnt/cargo/macsdk/ld64-link.sh <<'LD64'
#!/usr/bin/env bash
# Final-link driver for spur-cargo zigbuild (macOS cross): clang + ld64.lld
# against the synced macOS SDK. zig remains the C/C++ *compiler*; its own
# Mach-O linker fails with "relocation Overflow" on prebuilt Apple-clang
# objects (pyke onnxruntime) once libc++ becomes a dylib import. Needs a
# modern ld64.lld: prebuilt Apple objects reference objc_msgSend selector
# stubs (_objc_msgSend$sel), unknown to lld-14.
SDK=/mnt/cargo/macsdk/MacOSX.sdk
LLD=$(command -v ld64.lld-19 || command -v ld64.lld-16 || command -v ld64.lld-14 || command -v ld64.lld)
# -mlinker-version=705: Debian clang assumes an ancient host ld64 and emits
# the legacy -macosx_version_min flag, which ld64.lld rejects with "must
# specify -platform_version"; claiming a modern linker switches clang to
# the -platform_version form.
exec clang --target=arm64-apple-macos11 --sysroot="$SDK" \
    -fuse-ld="$LLD" -mlinker-version=705 \
    -F"$SDK/System/Library/Frameworks" -L"$SDK/usr/lib" \
    "$@"
LD64
chmod +x /mnt/cargo/macsdk/ld64-link.sh
command -v clang >/dev/null || echo "WARNING: clang missing on VM (apt install clang lld)"
command -v ld64.lld-19 >/dev/null 2>&1 || command -v ld64.lld-16 >/dev/null 2>&1 \
    || echo "WARNING: no modern ld64.lld (apt install lld-19); final spur link will fail on objc stubs"

echo "== libproc darwin bindings (stable copy + plant helper) =="
# libproc 0.14's build.rs gates bindgen behind #[cfg(target_os = "macos")] —
# a HOST cfg, so a Linux cross build runs the no-op linux main() and the lib
# then fails on the missing include. Pre-generate the bindings here; the
# plant helper copies them into whatever OUT_DIR hash the current flag set
# produced (the hash changes whenever RUSTFLAGS change).
SDK=/mnt/cargo/macsdk/MacOSX.sdk
printf '#include <libproc.h>\n' > /tmp/libproc_rs.h
bindgen /tmp/libproc_rs.h \
    --rust-target 1.72 --rust-edition 2018 --no-layout-tests \
    -o /mnt/cargo/macsdk/osx_libproc_bindings.rs \
    -- -x c++ -target aarch64-apple-darwin -isysroot "$SDK" -I"$SDK/usr/include" \
    2>/dev/null || bindgen /tmp/libproc_rs.h \
    --rust-target 1.72 --rust-edition 2018 --no-layout-tests \
    -o /mnt/cargo/macsdk/osx_libproc_bindings.rs \
    -- -x c++ -target aarch64-apple-darwin -isysroot "$SDK" -I"$SDK/usr/include"

cat > /mnt/cargo/macsdk/plant-libproc-bindings.sh <<'PLANT'
#!/usr/bin/env bash
# Copy the pre-generated libproc darwin bindings into every libproc OUT_DIR
# under the darwin target. Run after a build fails with
# "couldn't read .../libproc-<hash>/out/osx_libproc_bindings.rs", then rebuild.
set -euo pipefail
planted=0
for out in "$HOME"/spur/main/target/aarch64-apple-darwin/*/build/libproc-*/out; do
    [ -d "$out" ] || continue
    cp /mnt/cargo/macsdk/osx_libproc_bindings.rs "$out/"
    echo "planted: $out"
    planted=1
done
[ "$planted" = 1 ] || echo "no libproc OUT_DIR found yet — run the build once first"
PLANT
chmod +x /mnt/cargo/macsdk/plant-libproc-bindings.sh
wc -c /mnt/cargo/macsdk/osx_libproc_bindings.rs
echo "PROVISION_DONE"
REMOTE_EOF
rsync -az -e "$TRANSPORT" "$PROVISION_LOCAL" "$REMOTE_HOST:$REMOTE_PROVISION"
rm -f "$PROVISION_LOCAL"

log "Running remote provisioning..."
remote_ssh --command="bash $REMOTE_PROVISION"
log "Done. Build with: scripts/spur-cargo zigbuild --release -p spur-cli"
log "If the build fails on libproc's missing osx_libproc_bindings.rs, run the"
log "plant helper on the VM (bash /mnt/cargo/macsdk/plant-libproc-bindings.sh)"
log "and rebuild — see the POC doc for why."
