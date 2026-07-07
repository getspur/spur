#!/usr/bin/env bash
set -euo pipefail

PINNED_VHS_VERSION="0.11.0"

usage() {
  cat <<'USAGE'
Usage: scripts/e2e/vhs/check-vhs.sh [--install]

Checks the pinned VHS toolchain used by the SPUR TUI VHS spike.

  --install  Install vhs through Homebrew before checking. The script still
             verifies the installed version is exactly 0.11.0.

Manual fallback when Homebrew is unsuitable:
  Download https://github.com/charmbracelet/vhs/releases/download/v0.11.0/vhs_0.11.0_<OS>_<ARCH>.tar.gz
  and install ttyd plus ffmpeg separately, then re-run this check.
USAGE
}

install=false
if [[ "${1:-}" == "--install" ]]; then
  install=true
elif [[ $# -gt 0 ]]; then
  usage >&2
  exit 2
fi

if [[ "$install" == true ]]; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "error: --install requires Homebrew; install vhs ${PINNED_VHS_VERSION}, ttyd, and ffmpeg manually" >&2
    exit 1
  fi
  brew install vhs
fi

for program in vhs ttyd ffmpeg; do
  if ! command -v "$program" >/dev/null 2>&1; then
    echo "error: missing ${program}; install vhs ${PINNED_VHS_VERSION} with ttyd and ffmpeg on PATH" >&2
    exit 1
  fi
done

version_output="$(vhs --version)"
case "$version_output" in
  *" ${PINNED_VHS_VERSION}"|*" ${PINNED_VHS_VERSION}"$|*"version ${PINNED_VHS_VERSION}"*)
    ;;
  *)
    echo "error: expected vhs ${PINNED_VHS_VERSION}, got: ${version_output}" >&2
    exit 1
    ;;
esac

echo "$version_output"
ttyd --version
ffmpeg -version | sed -n '1p'
