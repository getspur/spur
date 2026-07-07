#!/usr/bin/env bash
set -euo pipefail

SHELL_USE_VERSION="0.0.1-beta.3"
SHELL_USE_TAG="v${SHELL_USE_VERSION}"
SHELL_USE_REPO="microsoft/shell-use"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"
install_base="${SHELL_USE_INSTALL_DIR:-"$repo_root/.spur/tmp/shell-use/$SHELL_USE_VERSION"}"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os:$arch" in
    Darwin:arm64|Darwin:aarch64)
      printf '%s|%s\n' \
        "aarch64-apple-darwin" \
        "cf6515e7400137dc0552c2f065fb416029dbe835d61dcd1bca6bbfdec58c3eee"
      ;;
    Darwin:x86_64)
      printf '%s|%s\n' \
        "x86_64-apple-darwin" \
        "053fde4fd4590df5719fe520b8ba26aa8f4adee4eeb3e852a7ebbc0f41f1da61"
      ;;
    Linux:aarch64|Linux:arm64)
      printf '%s|%s\n' \
        "aarch64-unknown-linux-gnu" \
        "72c93600c8870f8e90dbc5febbfe39a4f9a67d4bc482fb5969b57f5d7cc5a7fa"
      ;;
    Linux:x86_64)
      printf '%s|%s\n' \
        "x86_64-unknown-linux-gnu" \
        "a006147c618000295c98c8d1017bd64bc7705670c6eeb61de50f4c8e148eb3d8"
      ;;
    *)
      printf 'unsupported platform for shell-use release asset: %s %s\n' "$os" "$arch" >&2
      exit 2
      ;;
  esac
}

target_and_sha="$(detect_target)"
target="${target_and_sha%%|*}"
expected_sha="${target_and_sha##*|}"
asset="shell-use-${target}.tar.gz"
url="https://github.com/${SHELL_USE_REPO}/releases/download/${SHELL_USE_TAG}/${asset}"
bin_dir="$install_base/$target/bin"
bin_path="$bin_dir/shell-use"

if [[ -x "$bin_path" ]]; then
  version_output="$("$bin_path" --version 2>/dev/null || true)"
  if [[ "$version_output" == *"$SHELL_USE_VERSION"* ]]; then
    printf '%s\n' "$bin_path"
    exit 0
  fi
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/shell-use-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

archive="$tmp_dir/$asset"
extract_dir="$tmp_dir/extract"
mkdir -p "$extract_dir" "$bin_dir"

printf 'Installing shell-use %s for %s\n' "$SHELL_USE_VERSION" "$target" >&2
curl -fL --retry 3 --retry-delay 2 -o "$archive" "$url"

actual_sha="$(sha256_file "$archive")"
if [[ "$actual_sha" != "$expected_sha" ]]; then
  printf 'shell-use archive checksum mismatch\n' >&2
  printf 'expected: %s\nactual:   %s\n' "$expected_sha" "$actual_sha" >&2
  exit 1
fi

tar -xzf "$archive" -C "$extract_dir"
if [[ ! -f "$extract_dir/shell-use" ]]; then
  printf 'shell-use archive did not contain the expected shell-use binary\n' >&2
  exit 1
fi

install -m 0755 "$extract_dir/shell-use" "$bin_path"
printf '%s\n' "$bin_path"
