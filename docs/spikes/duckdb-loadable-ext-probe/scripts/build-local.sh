#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

EXTENSION_NAME="${EXTENSION_NAME:-spur_probe}"
DUCKDB_C_API_VERSION="${DUCKDB_C_API_VERSION:-v1.2.0}"
DUCKDB_PLATFORM="${DUCKDB_PLATFORM:-osx_arm64}"
EXTENSION_VERSION="${EXTENSION_VERSION:-0.1.0}"

cd "${PROBE_DIR}"

case "$(uname -s)" in
  Darwin) LIB_EXT="dylib" ;;
  Linux) LIB_EXT="so" ;;
  *)
    echo "Unsupported host OS for this spike: $(uname -s)" >&2
    exit 1
    ;;
esac

mkdir -p build/release

cargo build --release

python3 scripts/append_extension_metadata.py \
  --library-file "target/release/lib${EXTENSION_NAME}.${LIB_EXT}" \
  --extension-name "${EXTENSION_NAME}" \
  --out-file "build/release/${EXTENSION_NAME}.duckdb_extension" \
  --duckdb-platform "${DUCKDB_PLATFORM}" \
  --duckdb-version "${DUCKDB_C_API_VERSION}" \
  --extension-version "${EXTENSION_VERSION}"

printf '%s\n' "${PROBE_DIR}/build/release/${EXTENSION_NAME}.duckdb_extension"
