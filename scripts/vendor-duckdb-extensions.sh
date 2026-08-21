#!/usr/bin/env bash
# Download signed DuckDB core + community extension binaries for offline LOAD.
#
# Users never hit extensions.duckdb.org / community-extensions.duckdb.org:
# CI or `cargo xtask dist` runs this once, then spur sets extension_directory
# and LOADs from disk (see spur_analyst::analyst_extension_bootstrap_sql).
#
# Usage:
#   scripts/vendor-duckdb-extensions.sh --out dist/duckdb-extensions
#   scripts/vendor-duckdb-extensions.sh --platform osx_arm64 --out ./duckdb-extensions
#
# Layout written:
#   <out>/v1.4.4/<platform>/<name>.duckdb_extension
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Must match workspace duckdb = "=1.4.4" (community onager/duckpgq 404 on 1.4.5+).
DUCKDB_VERSION="${DUCKDB_VERSION:-1.4.4}"
OUT_DIR=""
PLATFORMS=()

CORE_EXTENSIONS=(lance fts icu)
COMMUNITY_EXTENSIONS=(onager duckpgq)

usage() {
  cat <<EOF
Usage: $(basename "$0") --out DIR [--platform PLATFORM]...

  --out DIR          Destination root (DuckDB extension_directory)
  --platform NAME    DuckDB platform (osx_arm64, osx_amd64, linux_arm64,
                     linux_amd64, windows_amd64). Repeatable. Default: host.
  --version X.Y.Z    DuckDB ABI (default: ${DUCKDB_VERSION})
EOF
}

host_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}:${arch}" in
    Darwin:arm64) echo osx_arm64 ;;
    Darwin:x86_64) echo osx_amd64 ;;
    Linux:aarch64|Linux:arm64) echo linux_arm64 ;;
    Linux:x86_64) echo linux_amd64 ;;
    MINGW*|MSYS*|CYGWIN*) echo windows_amd64 ;;
    *)
      echo "unsupported host ${os}:${arch}; pass --platform" >&2
      exit 1
      ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      OUT_DIR="$2"
      shift 2
      ;;
    --platform)
      PLATFORMS+=("$2")
      shift 2
      ;;
    --version)
      DUCKDB_VERSION="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "${OUT_DIR}" ]]; then
  echo "--out is required" >&2
  usage >&2
  exit 1
fi

if [[ ${#PLATFORMS[@]} -eq 0 ]]; then
  PLATFORMS=("$(host_platform)")
fi

mkdir -p "${OUT_DIR}"
OUT_DIR="$(cd "${OUT_DIR}" && pwd)"

download_one() {
  local repo="$1"
  local platform="$2"
  local name="$3"
  local dest_dir="${OUT_DIR}/v${DUCKDB_VERSION}/${platform}"
  local dest="${dest_dir}/${name}.duckdb_extension"
  local url="https://${repo}/v${DUCKDB_VERSION}/${platform}/${name}.duckdb_extension.gz"
  local gz_tmp dest_tmp http
  mkdir -p "${dest_dir}"
  if [[ -f "${dest}" ]]; then
    echo "exists ${dest}"
    return 0
  fi
  echo "download ${url}"
  gz_tmp="${dest}.gz.tmp"
  dest_tmp="${dest}.tmp"
  # Do not pass curl -f: HTTP 404 is a per-extension gap (osx_amd64 lance/onager
  # on DuckDB 1.4.4), not a dist failure. Transport errors still fail the job.
  http="$(curl -sS -L -o "${gz_tmp}" -w "%{http_code}" "${url}")"
  case "${http}" in
    200)
      gunzip -c "${gz_tmp}" > "${dest_tmp}"
      rm -f "${gz_tmp}"
      mv "${dest_tmp}" "${dest}"
      ;;
    404)
      echo "skip 404 ${url}"
      rm -f "${gz_tmp}" "${dest_tmp}"
      ;;
    *)
      echo "download failed HTTP ${http} ${url}" >&2
      rm -f "${gz_tmp}" "${dest_tmp}"
      return 1
      ;;
  esac
}

for platform in "${PLATFORMS[@]}"; do
  for name in "${CORE_EXTENSIONS[@]}"; do
    download_one "extensions.duckdb.org" "${platform}" "${name}"
  done
  for name in "${COMMUNITY_EXTENSIONS[@]}"; do
    download_one "community-extensions.duckdb.org" "${platform}" "${name}"
  done
done

shopt -s nullglob
vendored=("${OUT_DIR}/v${DUCKDB_VERSION}"/*/*.duckdb_extension)
if [[ ${#vendored[@]} -eq 0 ]]; then
  echo "no DuckDB extensions vendored into ${OUT_DIR}" >&2
  exit 1
fi

echo "vendored into ${OUT_DIR}"
echo "Set SPUR_DUCKDB_EXTENSION_DIR=${OUT_DIR} or copy this tree next to the spur binary as duckdb-extensions/"
# Keep repo root referenced so shellcheck knows this is a workspace helper.
: "${REPO_ROOT}"
