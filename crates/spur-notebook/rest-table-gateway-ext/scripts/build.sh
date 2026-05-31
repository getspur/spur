#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

EXTENSION_NAME="${EXTENSION_NAME:-spur_rest}"
DUCKDB_C_API_VERSION="${DUCKDB_C_API_VERSION:-v1.2.0}"
EXTENSION_VERSION="${EXTENSION_VERSION:-0.1.0}"

case "$(uname -s)" in
  Darwin)
    OS_PART="osx"
    LIB_EXT="dylib"
    ;;
  Linux)
    OS_PART="linux"
    LIB_EXT="so"
    ;;
  *)
    echo "Unsupported host OS: $(uname -s)" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64|amd64)
    ARCH_PART="amd64"
    ;;
  arm64|aarch64)
    ARCH_PART="arm64"
    ;;
  *)
    echo "Unsupported host architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

DUCKDB_PLATFORM="${DUCKDB_PLATFORM:-${OS_PART}_${ARCH_PART}}"

cd "${EXT_DIR}"

mkdir -p build/release

cargo build --release

TARGET_DIR="${CARGO_TARGET_DIR:-target}"

python3 scripts/append_extension_metadata.py \
  --library-file "${TARGET_DIR}/release/lib${EXTENSION_NAME}.${LIB_EXT}" \
  --extension-name "${EXTENSION_NAME}" \
  --out-file "build/release/${EXTENSION_NAME}.duckdb_extension" \
  --duckdb-platform "${DUCKDB_PLATFORM}" \
  --duckdb-version "${DUCKDB_C_API_VERSION}" \
  --extension-version "${EXTENSION_VERSION}"

INSTALL_DIR="${SPUR_EXT_INSTALL_DIR:-${HOME}/.spur/extensions}"
INSTALL_FILE="${INSTALL_DIR}/${EXTENSION_NAME}-${DUCKDB_PLATFORM}.duckdb_extension"
mkdir -p "${INSTALL_DIR}"
cp "build/release/${EXTENSION_NAME}.duckdb_extension" "${INSTALL_FILE}"

printf '%s\n' "${EXT_DIR}/build/release/${EXTENSION_NAME}.duckdb_extension"
