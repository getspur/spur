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

BUILT_EXTENSION="build/release/${EXTENSION_NAME}.duckdb_extension"

python3 scripts/append_extension_metadata.py \
  --library-file "${TARGET_DIR}/release/lib${EXTENSION_NAME}.${LIB_EXT}" \
  --extension-name "${EXTENSION_NAME}" \
  --out-file "${BUILT_EXTENSION}" \
  --duckdb-platform "${DUCKDB_PLATFORM}" \
  --duckdb-version "${DUCKDB_C_API_VERSION}" \
  --extension-version "${EXTENSION_VERSION}"

# Re-sign on macOS. The linker ad-hoc-signs lib*.dylib, but
# append_extension_metadata.py then appends DuckDB's footer AFTER the signed
# Mach-O, which invalidates that signature. On Apple Silicon every mapped
# executable page must carry a valid signature, so AMFI SIGKILLs any process
# that dlopen()s an unsigned/stale extension — including the notebook kernel.
# (DuckDB's `allow_unsigned_extensions` only relaxes DuckDB's own check, not the
# OS one.) Ad-hoc re-signing the final file — footer included — is what makes it
# loadable; it must run after the append, never before.
if [ "$(uname -s)" = "Darwin" ]; then
  codesign --remove-signature "${BUILT_EXTENSION}" 2>/dev/null || true
  # codesign re-verifies after signing and returns nonzero ("main executable
  # failed strict validation") for DuckDB extensions: its strict self-check
  # rejects the metadata footer that trails the signed Mach-O. The ad-hoc
  # signature it writes is nonetheless valid for dlopen(), so tolerate that exit
  # and instead assert a signature is actually present — a genuine signing
  # failure leaves the file unsigned and still aborts the build here. The load
  # smoke test below is the final gate.
  codesign --sign - --force "${BUILT_EXTENSION}" || true
  if ! codesign -dv "${BUILT_EXTENSION}" >/dev/null 2>&1; then
    echo "[build] ERROR: ${BUILT_EXTENSION} is unsigned after the codesign step" >&2
    exit 1
  fi
fi

# Build-time guard: actually dlopen the extension and run a trivial query, so a
# future signing/packaging regression fails the build instead of shipping a
# kernel-crashing artifact. A bad signature SIGKILLs this child (exit 137),
# which set -e turns into a build failure. Skipped only when duckdb is absent
# from the build host's python3.
if python3 -c "import duckdb" 2>/dev/null; then
  echo "[build] smoke-testing extension load..."
  python3 - "${BUILT_EXTENSION}" <<'PY'
import sys, duckdb
ext = sys.argv[1]
con = duckdb.connect(database=":memory:", config={"allow_unsigned_extensions": "true"})
con.execute(f"LOAD '{ext}'")
con.execute("SELECT 1").fetchone()
print("[build] extension loaded OK:", ext)
PY
else
  echo "[build] duckdb not importable on build host; skipping load smoke test" >&2
fi

INSTALL_DIR="${SPUR_EXT_INSTALL_DIR:-${HOME}/.spur/extensions}"
INSTALL_FILE="${INSTALL_DIR}/${EXTENSION_NAME}.duckdb_extension"
mkdir -p "${INSTALL_DIR}"
# Atomic replace via a fresh inode. Overwriting the existing file in place keeps
# its vnode, and macOS caches the previous (invalid) code-signature verdict for
# that vnode — so an in-place `cp` can leave the kernel still crashing even after
# the bytes are fixed. tmp-file + mv gives the install path a brand-new inode.
INSTALL_TMP="${INSTALL_FILE}.tmp.$$"
cp "${BUILT_EXTENSION}" "${INSTALL_TMP}"
mv -f "${INSTALL_TMP}" "${INSTALL_FILE}"

printf '%s\n' "${EXT_DIR}/${BUILT_EXTENSION}"
