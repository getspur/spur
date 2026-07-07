#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
provision="$repo_root/scripts/e2e/provision-vm-toolchain.sh"
startup="$repo_root/scripts/cloud-build/startup-aws.sh"
bake="$repo_root/scripts/cloud-build/bake-ami.sh"
readme="$repo_root/scripts/cloud-build/README.md"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

[[ -x "$provision" ]] || fail "provision script is missing or not executable"

grep -q 'SHELL_USE_VERSION="0.0.1-beta.3"' "$provision" \
  || fail "shell-use version is not pinned"
grep -q 'VHS_VERSION="0.11.0"' "$provision" \
  || fail "vhs version is not pinned"
grep -q 'TTYD_VERSION="1.7.7"' "$provision" \
  || fail "ttyd fallback version is not pinned"
grep -q '247c72cf9b01f9ea06225f49f52c692e869e17378992ac4e7a6eae92f9ccc554' "$provision" \
  || fail "shell-use arm64 musl checksum is missing"
grep -q '08f6a88aa4de64d4097b0da720c89f2cd9c0de7af5a35feb84b644321747f36a' "$provision" \
  || fail "shell-use x86_64 musl checksum is missing"
grep -q 'af782cddbf844a377df6ea41c0e72339393fa021be3f6cb70a2f47d48675d92b' "$provision" \
  || fail "vhs arm64 checksum is missing"
grep -q '99cb634587eaae0473c1ea377db80c3a048c27f99fe0a7febb1a1e8cb7ee5009' "$provision" \
  || fail "vhs x86_64 checksum is missing"
grep -q 'b38acadd89d1d396a0f5649aa52c539edbad07f4bc7348b27b4f4b7219dd4165' "$provision" \
  || fail "ttyd arm64 checksum is missing"
grep -q '8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55' "$provision" \
  || fail "ttyd x86_64 checksum is missing"
grep -q 'install_ttyd' "$provision" \
  || fail "ttyd fallback installer is missing"
grep -q 'SPUR_CHROME_BIN=/usr/local/bin/google-chrome' "$provision" \
  || fail "chromium path is not pinned for rod/vhs"
grep -q 'chromium-headless-shell' "$provision" \
  || fail "chromium headless shell apt package is missing"
grep -q 'exec /usr/bin/chromium-headless-shell' "$provision" \
  || fail "google-chrome wrapper does not point at chromium headless shell"
grep -q 'wait_for_apt_locks' "$provision" \
  || fail "VM installer does not wait for apt/dpkg locks"
! grep -q 'unattended-upgr' "$provision" \
  || fail "apt/dpkg wait incorrectly matches unattended-upgrade shutdown helper"
! grep -q "RETURN" "$provision" \
  || fail "installer cleanup must not use RETURN traps"
! grep -q 'Output /' "$provision" \
  || fail "VHS smoke output must be relative"
grep -q 'grep -q .*ok' "$provision" \
  || fail "VHS smoke must assert the rendered ok output"
grep -q 'provider_remote_ssh' "$provision" \
  || fail "local wrapper does not use cloud-build ssh helpers"
grep -q 'wait_for_startup_done' "$provision" \
  || fail "local wrapper does not wait for startup completion"
grep -q -- '--publish-bundle' "$provision" \
  || fail "S3 self-restore publish mode is missing"
grep -q -- '--vm' "$provision" \
  || fail "VM install mode is missing"

grep -q 'e2e/toolchain/provision-vm-toolchain.sh' "$startup" \
  || fail "startup-aws does not restore the e2e toolchain bundle"
grep -q 'SPUR_E2E_TOOLCHAIN' "$startup" \
  || fail "startup-aws does not mark e2e toolchain restore"
grep -q 'provision-vm-toolchain.sh --vm' "$bake" \
  || fail "bake-ami does not run the e2e toolchain installer"
grep -q 'E2E TUI Toolchain' "$readme" \
  || fail "cloud-build README does not document the e2e toolchain"

printf 'ok: e2e VM toolchain static contract\n'
