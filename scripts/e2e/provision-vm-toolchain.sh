#!/usr/bin/env bash
# Provision the remote build VM with the TUI e2e driver toolchain.
#
# Default mode runs from a local worktree: it resolves scripts/cloud-build,
# connects to the active VM through the provider ssh helpers, uploads this
# script, and executes VM mode with sudo. VM mode performs only idempotent
# installs on the current machine.
set -euo pipefail

SHELL_USE_VERSION="0.0.1-beta.3"
SHELL_USE_TAG="v${SHELL_USE_VERSION}"
VHS_VERSION="0.11.0"
VHS_TAG="v${VHS_VERSION}"
TTYD_VERSION="1.7.7"
E2E_TOOLCHAIN_S3_KEY="e2e/toolchain/provision-vm-toolchain.sh"
REMOTE_PROVISION="/tmp/spur-e2e-provision-vm-toolchain.sh"
SPUR_CHROME_BIN=/usr/local/bin/google-chrome

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="${SCRIPT_PATH%/*}"
[[ "$SCRIPT_DIR" == "$SCRIPT_PATH" ]] && SCRIPT_DIR="."
SCRIPT_DIR="$(cd -- "$SCRIPT_DIR" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd -P)"

log() { echo "[e2e-toolchain] $*" >&2; }

usage() {
  cat <<'USAGE'
Usage: scripts/e2e/provision-vm-toolchain.sh [--publish-bundle] [--smoke]
       scripts/e2e/provision-vm-toolchain.sh --vm [--smoke]

Provision the active cloud-build VM with the pinned TUI e2e toolchain:
shell-use 0.0.1-beta.3, vhs 0.11.0, ttyd, ffmpeg, and chromium.

Options:
  --vm              Install on the current machine instead of using cloud-build ssh.
  --publish-bundle  After remote install, upload this script to S3 so
                    startup-aws.sh can restore the e2e toolchain on fresh Spot VMs.
  --smoke           Render a minimal headless VHS tape ("echo ok").
  -h, --help        Show this help.
USAGE
}

VM_MODE=0
PUBLISH_BUNDLE=0
RUN_SMOKE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --vm) VM_MODE=1; shift ;;
    --publish-bundle) PUBLISH_BUNDLE=1; shift ;;
    --smoke) RUN_SMOKE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) log "unknown option: $1"; usage >&2; exit 2 ;;
  esac
done

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

download_verified() {
  local url="$1" expected_sha="$2" output="$3"
  curl -fsSL --retry 3 --retry-delay 2 -o "$output" "$url"
  local actual_sha
  actual_sha="$(sha256_file "$output")"
  if [[ "$actual_sha" != "$expected_sha" ]]; then
    log "checksum mismatch for $url"
    log "expected: $expected_sha"
    log "actual:   $actual_sha"
    return 1
  fi
}

linux_arch() {
  case "$(uname -m)" in
    aarch64|arm64) printf 'arm64\n' ;;
    x86_64|amd64) printf 'x86_64\n' ;;
    *) log "unsupported Linux architecture: $(uname -m)"; exit 2 ;;
  esac
}

shell_use_target_and_sha() {
  case "$(linux_arch)" in
    arm64)
      printf '%s|%s\n' \
        "aarch64-unknown-linux-musl" \
        "247c72cf9b01f9ea06225f49f52c692e869e17378992ac4e7a6eae92f9ccc554"
      ;;
    x86_64)
      printf '%s|%s\n' \
        "x86_64-unknown-linux-musl" \
        "08f6a88aa4de64d4097b0da720c89f2cd9c0de7af5a35feb84b644321747f36a"
      ;;
  esac
}

vhs_asset_and_sha() {
  case "$(linux_arch)" in
    arm64)
      printf '%s|%s\n' \
        "vhs_${VHS_VERSION}_Linux_arm64.tar.gz" \
        "af782cddbf844a377df6ea41c0e72339393fa021be3f6cb70a2f47d48675d92b"
      ;;
    x86_64)
      printf '%s|%s\n' \
        "vhs_${VHS_VERSION}_Linux_x86_64.tar.gz" \
        "99cb634587eaae0473c1ea377db80c3a048c27f99fe0a7febb1a1e8cb7ee5009"
      ;;
  esac
}

ttyd_asset_and_sha() {
  case "$(linux_arch)" in
    arm64)
      printf '%s|%s\n' \
        "ttyd.aarch64" \
        "b38acadd89d1d396a0f5649aa52c539edbad07f4bc7348b27b4f4b7219dd4165"
      ;;
    x86_64)
      printf '%s|%s\n' \
        "ttyd.x86_64" \
        "8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55"
      ;;
  esac
}

install_shell_use() {
  local version_output
  version_output="$(shell-use --version 2>/dev/null || true)"
  if [[ "$version_output" == *"$SHELL_USE_VERSION"* ]]; then
    log "shell-use already at $SHELL_USE_VERSION"
    return 0
  fi

  local target_and_sha target expected_sha asset url tmp_dir archive extract_dir
  target_and_sha="$(shell_use_target_and_sha)"
  target="${target_and_sha%%|*}"
  expected_sha="${target_and_sha##*|}"
  asset="shell-use-${target}.tar.gz"
  url="https://github.com/microsoft/shell-use/releases/download/${SHELL_USE_TAG}/${asset}"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spur-shell-use.XXXXXX")"
  archive="$tmp_dir/$asset"
  extract_dir="$tmp_dir/extract"
  mkdir -p "$extract_dir"

  log "installing shell-use $SHELL_USE_VERSION for $target"
  download_verified "$url" "$expected_sha" "$archive"
  tar -xzf "$archive" -C "$extract_dir"
  [[ -f "$extract_dir/shell-use" ]] || { log "shell-use archive missing binary"; return 1; }
  install -m 0755 "$extract_dir/shell-use" /usr/local/bin/shell-use
  rm -rf "$tmp_dir"
}

install_vhs() {
  local version_output
  version_output="$(vhs --version 2>/dev/null || true)"
  if [[ "$version_output" == *"$VHS_VERSION"* ]]; then
    log "vhs already at $VHS_VERSION"
    return 0
  fi

  local asset_and_sha asset expected_sha url tmp_dir archive extract_dir binary
  asset_and_sha="$(vhs_asset_and_sha)"
  asset="${asset_and_sha%%|*}"
  expected_sha="${asset_and_sha##*|}"
  url="https://github.com/charmbracelet/vhs/releases/download/${VHS_TAG}/${asset}"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spur-vhs.XXXXXX")"
  archive="$tmp_dir/$asset"
  extract_dir="$tmp_dir/extract"
  mkdir -p "$extract_dir"

  log "installing vhs $VHS_VERSION from $asset"
  download_verified "$url" "$expected_sha" "$archive"
  tar -xzf "$archive" -C "$extract_dir"
  binary="$(find "$extract_dir" -type f -name vhs -perm -0100 | head -1)"
  [[ -n "$binary" ]] || { log "vhs archive missing binary"; return 1; }
  install -m 0755 "$binary" /usr/local/bin/vhs
  rm -rf "$tmp_dir"
}

install_ttyd() {
  local version_output
  version_output="$(ttyd --version 2>/dev/null || true)"
  if [[ "$version_output" == *"$TTYD_VERSION"* ]]; then
    log "ttyd already at $TTYD_VERSION"
    return 0
  fi

  local asset_and_sha asset expected_sha url tmp_dir binary
  asset_and_sha="$(ttyd_asset_and_sha)"
  asset="${asset_and_sha%%|*}"
  expected_sha="${asset_and_sha##*|}"
  url="https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/${asset}"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spur-ttyd.XXXXXX")"
  binary="$tmp_dir/$asset"

  log "installing ttyd $TTYD_VERSION from pinned upstream binary"
  download_verified "$url" "$expected_sha" "$binary"
  install -m 0755 "$binary" /usr/local/bin/ttyd
  rm -rf "$tmp_dir"
}

install_chromium_wrapper() {
  cat >/usr/local/bin/google-chrome <<'EOF'
#!/usr/bin/env bash
exec /usr/bin/chromium-headless-shell "$@"
EOF
  chmod 0755 /usr/local/bin/google-chrome
}

wait_for_apt_locks() {
  local waited=0 max_wait=900
  local locks=(
    /var/lib/dpkg/lock-frontend
    /var/lib/dpkg/lock
    /var/cache/apt/archives/lock
  )
  while :; do
    local holders=""
    if command -v fuser >/dev/null 2>&1; then
      local lock pids
      for lock in "${locks[@]}"; do
        [[ -e "$lock" ]] || continue
        pids="$(fuser "$lock" 2>/dev/null || true)"
        [[ -n "$pids" ]] && holders="${holders:+$holders }$pids"
      done
    elif pgrep -x 'apt|apt-get|dpkg' >/dev/null 2>&1; then
      holders="$(pgrep -x 'apt|apt-get|dpkg' | tr '\n' ' ')"
    fi
    [[ -z "$holders" ]] && break

    if [[ $waited -eq 0 ]]; then
      log "waiting for apt/dpkg locks held by:$holders"
    fi
    if [[ $waited -ge $max_wait ]]; then
      log "apt/dpkg still active after ${max_wait}s"
      ps -eo pid,ppid,comm,etimes,args | awk '/apt|dpkg/ && !/awk/ {print}' >&2 || true
      return 1
    fi
    sleep 5
    waited=$((waited + 5))
  done
}

apt_get() {
  apt-get -o DPkg::Lock::Timeout=600 "$@"
}

apt_has_candidate() {
  local package="$1"
  apt-cache policy "$package" | awk '/Candidate:/ && $2 != "(none)" { found=1 } END { exit(found ? 0 : 1) }'
}

install_apt_tools() {
  local missing=() hold_pkgs=() pkg
  command -v ffmpeg >/dev/null 2>&1 || missing+=(ffmpeg)
  command -v chromium >/dev/null 2>&1 || missing+=(chromium)
  command -v chromium-headless-shell >/dev/null 2>&1 || missing+=(chromium-headless-shell)

  if [[ ${#missing[@]} -gt 0 ]] || ! command -v ttyd >/dev/null 2>&1; then
    wait_for_apt_locks
    apt_get update
  fi

  if ! command -v ttyd >/dev/null 2>&1; then
    if apt_has_candidate ttyd; then
      missing+=(ttyd)
    else
      log "apt has no ttyd candidate; using pinned upstream ttyd $TTYD_VERSION"
      install_ttyd
    fi
  fi

  if [[ ${#missing[@]} -gt 0 ]]; then
    log "installing apt tools: ${missing[*]}"
    apt_get install -y --no-install-recommends "${missing[@]}"
  else
    log "apt tools already present"
  fi

  install -d -m 0755 /opt/spur-e2e-toolchain
  : >/opt/spur-e2e-toolchain/apt-versions.txt
  for pkg in ttyd ffmpeg chromium chromium-headless-shell; do
    dpkg-query -W -f='${Package}=${Version}\n' "$pkg" \
      >>/opt/spur-e2e-toolchain/apt-versions.txt 2>/dev/null || true
    if dpkg-query -W "$pkg" >/dev/null 2>&1; then
      hold_pkgs+=("$pkg")
    fi
  done
  if command -v ttyd >/dev/null 2>&1 && ! dpkg-query -W ttyd >/dev/null 2>&1; then
    printf 'ttyd=%s (github release)\n' "$TTYD_VERSION" \
      >>/opt/spur-e2e-toolchain/apt-versions.txt
  fi
  [[ ${#hold_pkgs[@]} -eq 0 ]] || apt-mark hold "${hold_pkgs[@]}" >/dev/null
  install_chromium_wrapper
}

write_profile() {
  cat >/etc/profile.d/spur-e2e-toolchain.sh <<EOF
# SPUR TUI e2e toolchain. Managed by scripts/e2e/provision-vm-toolchain.sh.
export SPUR_CHROME_BIN=/usr/local/bin/google-chrome
export CHROME_BIN=/usr/local/bin/google-chrome
export ROD_BROWSER_BIN=/usr/local/bin/google-chrome
export PATH="/usr/local/bin:\$PATH"
EOF
  chmod 0644 /etc/profile.d/spur-e2e-toolchain.sh
}

verify_toolchain() {
  local shell_use_version vhs_version
  shell_use_version="$(shell-use --version)"
  vhs_version="$(vhs --version)"
  [[ "$shell_use_version" == *"$SHELL_USE_VERSION"* ]] || {
    log "expected shell-use $SHELL_USE_VERSION, got: $shell_use_version"; return 1;
  }
  [[ "$vhs_version" == *"$VHS_VERSION"* ]] || {
    log "expected vhs $VHS_VERSION, got: $vhs_version"; return 1;
  }
  command -v ttyd >/dev/null
  command -v ffmpeg >/dev/null
  command -v chromium >/dev/null
  command -v chromium-headless-shell >/dev/null
  [[ -x "$SPUR_CHROME_BIN" ]]

  printf '%s\n' "$shell_use_version"
  printf '%s\n' "$vhs_version"
  ttyd --version
  ffmpeg -version | sed -n '1p'
  chromium --version
  "$SPUR_CHROME_BIN" --version
}

run_vhs_smoke() {
  local smoke_dir tape output old_pwd
  smoke_dir="$(mktemp -d "${TMPDIR:-/tmp}/spur-vhs-smoke.XXXXXX")"
  tape="$smoke_dir/echo-ok.tape"
  output="echo-ok.txt"
  cat >"$tape" <<EOF
Output $output

Set Shell bash
Set Width 480
Set Height 160
Set FontSize 18
Set TypingSpeed 0ms

Type "echo ok"
Enter
Wait+Screen@5s /ok/
EOF
  log "running VHS headless smoke tape"
  old_pwd="$PWD"
  cd "$smoke_dir"
  VHS_NO_SANDBOX=1 vhs "$(basename "$tape")"
  grep -q 'ok' "$output"
  printf 'smoke_output=%s/%s\n' "$smoke_dir" "$output"
  grep -n 'ok' "$output" | sed -n '1,20p'
  cd "$old_pwd"
  rm -rf "$smoke_dir"
}

install_on_vm() {
  [[ "$(uname -s)" == "Linux" ]] || { log "VM mode only supports Linux"; exit 2; }
  if [[ $EUID -ne 0 ]]; then
    args=(--vm)
    [[ $RUN_SMOKE -eq 1 ]] && args+=(--smoke)
    exec sudo -E bash "$SCRIPT_PATH" "${args[@]}"
  fi

  export DEBIAN_FRONTEND=noninteractive
  install_apt_tools
  install_shell_use
  install_vhs
  write_profile
  verify_toolchain
  if [[ $RUN_SMOKE -eq 1 ]]; then run_vhs_smoke; fi
}

resolve_cloud_build_dir() {
  local candidate git_toplevel repo_root notebook_repo
  if [[ -n "${SPUR_CLOUD_BUILD_SH:-}" ]]; then
    candidate="$(dirname "$SPUR_CLOUD_BUILD_SH")"
    [[ -e "$candidate/build.sh" ]] && { cd -- "$candidate" && pwd -P; return 0; }
    return 1
  fi
  candidate="$REPO_ROOT/scripts/cloud-build"
  [[ -e "$candidate/build.sh" ]] && { cd -- "$candidate" && pwd -P; return 0; }
  if [[ -n "${SPUR_NOTEBOOK_REPO:-}" ]]; then
    candidate="$SPUR_NOTEBOOK_REPO/scripts/cloud-build"
    [[ -e "$candidate/build.sh" ]] && { cd -- "$candidate" && pwd -P; return 0; }
  fi
  git_toplevel="$(git -C "$REPO_ROOT" rev-parse --show-toplevel 2>/dev/null || true)"
  [[ -n "$git_toplevel" ]] || return 1
  if [[ "$git_toplevel" == *"/.spur/worktrees/"* ]]; then
    repo_root="$(dirname "$(dirname "$(dirname "$git_toplevel")")")"
  else
    repo_root="$git_toplevel"
  fi
  notebook_repo="$(dirname "$repo_root")/spur-notebook"
  candidate="$notebook_repo/scripts/cloud-build"
  [[ -e "$candidate/build.sh" ]] && { cd -- "$candidate" && pwd -P; return 0; }
  return 1
}

remote_install() {
  local cb remote_args remote_arg_string s3_uri
  cb="$(resolve_cloud_build_dir)" || {
    log "cannot find scripts/cloud-build; set SPUR_CLOUD_BUILD_SH or SPUR_NOTEBOOK_REPO"
    exit 2
  }

  # provider_choose_transport expects SCRIPT_DIR to be the cloud-build dir.
  SCRIPT_DIR="$cb"
  # shellcheck disable=SC1091
  source "$cb/config.env"
  # shellcheck disable=SC1090
  source "$cb/provider-${SPUR_CLOUD}.sh"
  provider_choose_transport
  log "VM: $REMOTE_HOST via $TRANSPORT_MODE"
  wait_for_startup_done

  log "uploading provisioner to $REMOTE_HOST:$REMOTE_PROVISION"
  rsync -az -e "$TRANSPORT" "$SCRIPT_PATH" "$REMOTE_HOST:$REMOTE_PROVISION"

  remote_args=(--vm)
  [[ $RUN_SMOKE -eq 1 ]] && remote_args+=(--smoke)
  remote_arg_string="$(printf ' %q' "${remote_args[@]}")"
  provider_remote_ssh --command="sudo bash $(printf '%q' "$REMOTE_PROVISION")$remote_arg_string"

  if [[ $PUBLISH_BUNDLE -eq 1 ]]; then
    if [[ "$SPUR_CLOUD" != aws* ]]; then
      log "--publish-bundle currently requires an AWS cloud with S3"
      exit 2
    fi
    s3_uri="s3://${SCCACHE_BUCKET}/${E2E_TOOLCHAIN_S3_KEY}"
    log "publishing provisioner to $s3_uri"
    provider_remote_ssh --command="aws s3 cp --region $(printf '%q' "$SCCACHE_S3_REGION") $(printf '%q' "$REMOTE_PROVISION") $(printf '%q' "$s3_uri")"
    log "published; fresh AWS Spot VMs restore it from startup-aws.sh"
  fi
}

wait_for_startup_done() {
  log "waiting for VM startup marker"
  provider_remote_ssh --command='
    for _ in $(seq 1 180); do
      if grep -q "startup done" /var/log/spur-startup.log 2>/dev/null; then
        echo "startup-script complete"
        exit 0
      fi
      sleep 5
    done
    echo "startup-script did not finish in 15 min — check /var/log/spur-startup.log" >&2
    exit 1
  '
}

if [[ $VM_MODE -eq 1 ]]; then
  install_on_vm
else
  remote_install
fi
