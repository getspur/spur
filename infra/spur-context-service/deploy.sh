#!/usr/bin/env bash
# Build, package, and deploy spur-context-service Lambda.
#
# Usage:
#   ./deploy.sh                    # build on remote VM, package, terraform apply
#   ./deploy.sh --local-zip path   # skip build, use existing zip
#
# Prerequisites:
#   - scripts/spur-cargo (remote Graviton4 VM)
#   - terraform >= 1.5
#   - AWS credentials with Lambda/S3/IAM access
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INFRA_DIR="$SCRIPT_DIR"
BUILD_DIR=$(mktemp -d)
trap 'rm -rf "$BUILD_DIR"' EXIT

DUCKDB_VERSION="1.5.2"
EXT_PLATFORM="linux_arm64"
EXTENSIONS=("httpfs" "ducklake")

log() { echo "[deploy] $*" >&2; }

download_extensions() {
    local ext_dir="$BUILD_DIR/.duckdb/extensions/v${DUCKDB_VERSION}/${EXT_PLATFORM}"
    mkdir -p "$ext_dir"
    for ext in "${EXTENSIONS[@]}"; do
        local url="https://extensions.duckdb.org/v${DUCKDB_VERSION}/${EXT_PLATFORM}/${ext}.duckdb_extension.gz"
        local dest="$ext_dir/${ext}.duckdb_extension"
        if [[ ! -f "$dest" ]]; then
            log "downloading $ext..."
            curl -sL "$url" | gunzip > "$dest"
        fi
    done
}

build_binary() {
    log "building on remote Graviton4 VM..."
    cd "$REPO_ROOT"
    scripts/spur-cargo build -p spur-context-service --features lambda --release
    scripts/cloud-build/fetch.sh --to "$BUILD_DIR/bootstrap" target/release/spur-context-service
}

package_zip() {
    local zip_path="$1"
    log "packaging Lambda zip..."
    cp "$BUILD_DIR/bootstrap" "$BUILD_DIR/bootstrap"
    cd "$BUILD_DIR"
    zip -r "$zip_path" bootstrap .duckdb/ -x "*.gz"
    log "zip size: $(du -h "$zip_path" | cut -f1)"
}

main() {
    local zip_path="$REPO_ROOT/target/lambda/spur-context-service.zip"

    if [[ "${1:-}" == "--local-zip" ]]; then
        zip_path="$2"
    else
        mkdir -p "$(dirname "$zip_path")"
        download_extensions
        build_binary
        package_zip "$zip_path"
    fi

    log "running terraform..."
    cd "$INFRA_DIR"
    terraform init -upgrade
    terraform apply \
        -var "lambda_zip_path=$zip_path" \
        -auto-approve

    log "deployed. API URL:"
    terraform output -raw api_url
    echo ""
}

main "$@"
