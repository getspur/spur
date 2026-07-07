#!/usr/bin/env bash
# Provision the cloud-build VM for Windows (x86_64-pc-windows-msvc)
# cross-compiles driven by `scripts/spur-cargo xwin`.
#
# What it does (all ON the VM, nothing is copied from this machine):
#   1. apt-installs the LLVM 19 cl-mode toolchain: clang-cl (clang-tools-19),
#      lld-link (lld-19), llvm-lib/llvm-rc/llvm-dlltool (llvm-19).
#   2. Installs the pinned cargo-xwin release binary into
#      /mnt/cargo/cargo-home/bin and shadows Debian's unversioned LLD-14
#      lld-link with -19 symlinks there (that dir precedes /usr/bin on PATH).
#   3. Adds the x86_64-pc-windows-msvc rust-std to EVERY installed toolchain
#      (the repo pins via rust-toolchain.toml; adding only to `stable` leaves
#      the pinned toolchain without a windows std → E0463).
#   4. Appends the XWIN block (XWIN_CACHE_DIR on the big disk,
#      XWIN_ACCEPT_LICENSE=1) to /etc/profile.d/spur-build.sh. xwin downloads
#      the MSVC CRT + Windows SDK from Microsoft on first use (~300 MB).
#
# Idempotent — safe to re-run. Unlike the macOS cross (zigbuild-provision-vm.sh)
# there is no local-SDK copy and no S3 bundle: a fresh spot VM re-provisions
# itself entirely from apt/GitHub/Microsoft via startup-aws.sh's WINCROSS
# section (spur-notebook repo), which mirrors these steps.
#
# See: docs/superpowers/specs/2026-07-07-xwin-windows-cross-poc.md
set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="${SCRIPT_PATH%/*}"
[[ "$SCRIPT_DIR" == "$SCRIPT_PATH" ]] && SCRIPT_DIR="."
SCRIPT_DIR=$(cd "$SCRIPT_DIR" && pwd -P)

log() { echo "[xwin-provision] $*" >&2; }

CARGO_XWIN_VERSION="${SPUR_CARGO_XWIN_VERSION:-0.23.0}"

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

log "provisioning Windows cross toolchain (cargo-xwin $CARGO_XWIN_VERSION)..."
provider_remote_ssh --command="bash -lc 'set -euo pipefail
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y clang-tools-19 llvm-19 lld-19 >/dev/null
echo \"[vm] LLVM 19 cl-mode tools installed\"

cd /mnt/cargo/cargo-home/bin
if [ ! -x cargo-xwin ] || ! ./cargo-xwin --version | grep -q \"$CARGO_XWIN_VERSION\"; then
    curl -fsSL https://github.com/rust-cross/cargo-xwin/releases/download/v$CARGO_XWIN_VERSION/cargo-xwin-v$CARGO_XWIN_VERSION.aarch64-unknown-linux-musl.tar.gz | tar xz
    chmod +x cargo-xwin
fi
echo \"[vm] cargo-xwin: \$(./cargo-xwin --version)\"

# Debian installs versioned LLVM names only, and the base lld package owns
# the unversioned lld-link (LLD 14). This dir precedes /usr/bin on PATH, so
# -19 symlinks here win for cargo-xwin lookups by plain name.
for t in clang-cl lld-link llvm-lib llvm-rc llvm-dlltool; do
    [ -e /usr/bin/\$t-19 ] && ln -sf /usr/bin/\$t-19 \$t
done
echo \"[vm] cl-mode symlinks: \$(clang-cl --version | head -1)\"

for tc in \$(rustup toolchain list | awk \"{print \\\$1}\"); do
    rustup target add --toolchain \"\$tc\" x86_64-pc-windows-msvc >/dev/null 2>&1 || true
done
echo \"[vm] rust-std x86_64-pc-windows-msvc: \$(rustup target list --installed | grep -c windows) toolchain(s) confirm on default\"

mkdir -p /mnt/cargo/xwin
if ! grep -q XWIN_CACHE_DIR /etc/profile.d/spur-build.sh 2>/dev/null; then
    sudo tee -a /etc/profile.d/spur-build.sh >/dev/null <<EOF

# Windows (msvc) cross via cargo-xwin: MSVC CRT/SDK cache on the big disk.
export XWIN_CACHE_DIR=/mnt/cargo/xwin
export XWIN_ACCEPT_LICENSE=1
EOF
    echo \"[vm] profile.d XWIN block added\"
fi

# DirectML import lib: pyke's prebuilt onnxruntime bundles the DirectML
# execution provider, but DirectML.lib is NOT in the Windows SDK — it ships
# in the Microsoft.AI.DirectML NuGet redistributable (the DLL itself is an
# inbox Windows 10 1903+ component, so importing it is safe).
if [ ! -f /mnt/cargo/directml/bin/x64-win/DirectML.lib ]; then
    mkdir -p /mnt/cargo/directml
    curl -fsSL https://www.nuget.org/api/v2/package/Microsoft.AI.DirectML/1.15.4 -o /mnt/cargo/directml/directml.nupkg
    (cd /mnt/cargo/directml && python3 -m zipfile -e directml.nupkg .)
fi
echo \"[vm] DirectML redistributable staged\"

if ! grep -q 'PathCch.lib' /etc/profile.d/spur-build.sh 2>/dev/null; then
    sudo tee -a /etc/profile.d/spur-build.sh >/dev/null <<EOF
# Self-heal mixed-case import-lib names inside the xwin splat (idempotent):
# MSVC-built static libs (pyke onnxruntime) embed /DEFAULTLIB:PathCch.lib
# and xwin only generates original/lowercase/UPPERCASE name variants, which
# on a case-sensitive FS misses PascalCase requests. Re-splats on a fresh
# VM lose the link, so re-plant it on every dispatch shell.
[ -f /mnt/cargo/xwin/xwin/sdk/lib/um/x86_64/pathcch.lib ] && ln -sf pathcch.lib /mnt/cargo/xwin/xwin/sdk/lib/um/x86_64/PathCch.lib 2>/dev/null || true
# Same self-heal for the DirectML import lib (staged above; not part of the
# Windows SDK splat).
[ -f /mnt/cargo/directml/bin/x64-win/DirectML.lib ] && [ -d /mnt/cargo/xwin/xwin/sdk/lib/um/x86_64 ] && cp -n /mnt/cargo/directml/bin/x64-win/DirectML.lib /mnt/cargo/xwin/xwin/sdk/lib/um/x86_64/DirectML.lib 2>/dev/null || true
EOF
    echo \"[vm] profile.d PathCch/DirectML self-heal added\"
fi
echo \"[vm] provision complete\"'"

log "done. Build with: scripts/spur-cargo xwin build --release -p spur-cli"
log "Fetch with:      scripts/cloud-build/fetch.sh --via-s3 target/x86_64-pc-windows-msvc/release/spur.exe"
