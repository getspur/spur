#!/usr/bin/env bash
# Build deterministic arm64 provided.al2023 bootstrap ZIPs for the independent
# API-key authorizer and cleanup Lambdas.
set -euo pipefail

SCRIPT_DIR=$(cd "${BASH_SOURCE[0]%/*}" && pwd -P)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)
CRATE_DIR="$ROOT/crates/spur-context-service"
TARGET_TRIPLE="aarch64-unknown-linux-musl"
BUILD_TARGET_DIR="${SPUR_API_KEY_LAMBDA_TARGET_DIR:-$ROOT/target/context-api-key-lambdas}"
OUTPUT_DIR="$ROOT/target/lambda"

# Terraform defaults consume these exact repository-relative artifacts:
# target/lambda/spur-context-api-key-authorizer.zip
# target/lambda/spur-context-api-key-cleanup.zip
AUTHORIZER_ZIP="$OUTPUT_DIR/spur-context-api-key-authorizer.zip"
CLEANUP_ZIP="$OUTPUT_DIR/spur-context-api-key-cleanup.zip"

for command in cargo-zigbuild rustup zip; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "missing required packaging command: $command" >&2
        exit 1
    fi
done

if [[ ! -f "$CRATE_DIR/Cargo.lock" ]]; then
    echo "missing committed lockfile: crates/spur-context-service/Cargo.lock" >&2
    exit 1
fi

rustup target add "$TARGET_TRIPLE" >/dev/null
mkdir -p "$BUILD_TARGET_DIR" "$OUTPUT_DIR"

export CARGO_TARGET_DIR="$BUILD_TARGET_DIR"
export CARGO_PROFILE_RELEASE_STRIP="symbols"
export SOURCE_DATE_EPOCH="315532800"

build_bootstrap() {
    local binary="$1"
    local feature="$2"
    local output="$3"
    local executable="$BUILD_TARGET_DIR/$TARGET_TRIPLE/release/$binary"
    local stage="$BUILD_TARGET_DIR/lambda-stage/$binary"

    SPUR_REMOTE=0 "$ROOT/scripts/spur-cargo" --workdir "$CRATE_DIR" \
        zigbuild \
        --release \
        --target aarch64-unknown-linux-musl \
        --no-default-features \
        --features "$feature" \
        --bin "$binary" \
        --locked

    rm -rf "$stage"
    mkdir -p "$stage"
    cp "$executable" "$stage/bootstrap"
    chmod 0755 "$stage/bootstrap"
    touch -t 198001010000 "$stage/bootstrap"
    rm -f "$output"
    (
        cd "$stage"
        COPYFILE_DISABLE=1 zip -X -q "$output" bootstrap
    )
}

build_bootstrap \
    "spur-context-api-key-authorizer" \
    "api-key-authorizer" \
    "$AUTHORIZER_ZIP"
build_bootstrap \
    "spur-context-api-key-cleanup" \
    "api-key-cleanup" \
    "$CLEANUP_ZIP"

printf '%s\n' "$AUTHORIZER_ZIP" "$CLEANUP_ZIP"
