#!/usr/bin/env bash
# Regression checks for deployable arm64 CPU targets.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="$SCRIPT_DIR/graviton2-baseline.sh"
DEPLOY="$SCRIPT_DIR/deploy.sh"
WRAPPER="$SCRIPT_DIR/build-and-push-remote.sh"
WORKER_DOCKERFILE="$SCRIPT_DIR/Dockerfile.worker"
README="$SCRIPT_DIR/README.md"

fail() {
    echo "[graviton2-baseline-test] $*" >&2
    exit 1
}

[[ -f "$HELPER" ]] || fail "missing Graviton2 baseline helper: $HELPER"

# shellcheck source=infra/spur-context-service/graviton2-baseline.sh
source "$HELPER"

assert_graviton2_safe_flags \
    "test baseline" \
    "$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
    "$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS"

assert_graviton2_safe_flags \
    "generic armv8-a baseline" \
    "-Ctarget-cpu=generic -Ctarget-feature=+lse" \
    "-march=armv8-a+lse -O2" \
    "-march=armv8-a+lse -O2"

if assert_graviton2_safe_flags \
    "bad rust target" \
    "-Ctarget-cpu=neoverse-v2 -Ctarget-feature=+lse" \
    "$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" >/dev/null 2>&1; then
    fail "neoverse-v2 Rust target was accepted"
fi

if assert_graviton2_safe_flags \
    "missing rust target" \
    "-Ctarget-feature=+lse" \
    "$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" >/dev/null 2>&1; then
    fail "Rust flags without an explicit baseline target-cpu were accepted"
fi

if assert_graviton2_safe_flags \
    "conflicting rust target" \
    "-Ctarget-cpu=neoverse-n1 -Ctarget-cpu=native" \
    "$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" >/dev/null 2>&1; then
    fail "Rust flags with a later non-baseline target-cpu were accepted"
fi

if assert_graviton2_safe_flags \
    "bad C target" \
    "$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
    "-mcpu=native -O2" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" >/dev/null 2>&1; then
    fail "CFLAGS without a Graviton2-safe baseline were accepted"
fi

if assert_graviton2_safe_flags \
    "conflicting C target" \
    "$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
    "-mcpu=neoverse-n1 -mcpu=native -O2" \
    "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" >/dev/null 2>&1; then
    fail "CFLAGS with a later non-baseline CPU were accepted"
fi

for file in "$DEPLOY" "$WRAPPER" "$WORKER_DOCKERFILE" "$HELPER"; do
    if grep -q 'neoverse-v2' "$file"; then
        fail "deployable artifact path mentions neoverse-v2: $file"
    fi
done

grep -q 'graviton2-baseline.sh' "$DEPLOY" \
    || fail "deploy.sh does not source the Graviton2 baseline helper"
grep -q 'run_graviton2_safe_cargo "serving Lambda bootstrap"' "$DEPLOY" \
    || fail "serving Lambda bootstrap build is not guarded"
grep -q 'run_graviton2_safe_cargo "Fargate worker binary"' "$DEPLOY" \
    || fail "Fargate worker build is not guarded"
grep -q 'run_graviton2_safe_cargo "worker Lambda image binary"' "$DEPLOY" \
    || fail "worker Lambda image build is not guarded"
grep -q 'run_graviton2_safe_cargo "spur CLI worker image dependency"' "$DEPLOY" \
    || fail "worker image spur CLI build is not guarded"
grep -q -- '--worker-image-only' "$WRAPPER" \
    || fail "remote worker image wrapper no longer delegates to deploy.sh"
grep -q 'Graviton2-safe CPU baseline' "$README" \
    || fail "README does not document the deployable artifact CPU baseline"

echo "[graviton2-baseline-test] ok"
