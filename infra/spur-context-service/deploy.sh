#!/usr/bin/env bash
# Build, package, and deploy spur-context-service Lambda + indexing worker.
#
# Usage:
#   ./deploy.sh                    # build Lambda + worker, terraform apply
#   ./deploy.sh --local-zip path   # skip Lambda build, use existing zip
#   ./deploy.sh --skip-worker      # skip worker image build/push
#
# Prerequisites:
#   - scripts/spur-cargo (remote Graviton4 VM)
#   - terraform >= 1.5
#   - docker (for worker container build)
#   - AWS credentials with Lambda/S3/IAM/ECR/ECS/SFN access
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INFRA_DIR="$SCRIPT_DIR"
BUILD_DIR=$(mktemp -d)
trap 'rm -rf "$BUILD_DIR"' EXIT

DUCKDB_VERSION="1.5.4"
EXT_PLATFORM="linux_arm64"
EXTENSIONS=("httpfs" "ducklake")

# Worker container config.  The image is built on the remote VM (x86_64 for
# Fargate compatibility) and pushed to ECR.
WORKER_ECR_REPO="spur-context-worker"
WORKER_IMAGE_TAG="latest"
AWS_ACCOUNT_ID="$(aws sts get-caller-identity --query Account --output text)"
AWS_REGION_VAL="$(cd "$INFRA_DIR" && terraform output -raw aws_region 2>/dev/null || echo ap-southeast-5)"

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
    log "building on remote Graviton4 VM (portable arm64 for Lambda: neoverse-n1)..."
    cd "$REPO_ROOT"
    # spur-context-service is excluded from the workspace (standalone Cargo.toml
    # with duckdb 1.5.4). Build from the crate directory directly.
    AWS_RUSTFLAGS_DEFAULT="-Ctarget-cpu=neoverse-n1 -Ctarget-feature=+lse -Clinker=clang -Clink-arg=-fuse-ld=/mnt/cargo/rust-lld-driver/ld.lld" \
    CFLAGS="-mcpu=neoverse-n1 -O2" \
    CXXFLAGS="-mcpu=neoverse-n1 -O2" \
        scripts/spur-cargo run --workdir crates/spur-context-service -- cargo build --features lambda --release
    scripts/cloud-build/fetch.sh --to "$BUILD_DIR/bootstrap" crates/spur-context-service/target/release/spur-context-service
}

package_zip() {
    local zip_path="$1"
    log "packaging Lambda zip..."
    cd "$BUILD_DIR"
    zip -r "$zip_path" bootstrap .duckdb/ -x "*.gz"
    log "zip size: $(du -h "$zip_path" | cut -f1)"
}

build_worker() {
    log "building worker binary (--features worker, arm64 neoverse-n1 for Fargate)..."
    cd "$REPO_ROOT"
    # Fargate ARM64 runs on Graviton2 (neoverse-n1). The build VM's default
    # neoverse-v2 produces SIGILL on Fargate. Match the Lambda build flags.
    # The binary stays on the VM — build_and_push_worker_image() builds the
    # Docker image remotely using docker-build.sh, so no local fetch needed.
    AWS_RUSTFLAGS_DEFAULT="-Ctarget-cpu=neoverse-n1 -Ctarget-feature=+lse -Clinker=clang -Clink-arg=-fuse-ld=/mnt/cargo/rust-lld-driver/ld.lld" \
    CFLAGS="-mcpu=neoverse-n1 -O2" \
    CXXFLAGS="-mcpu=neoverse-n1 -O2" \
        scripts/spur-cargo build -p spur-context-service --features worker --release
}

build_and_push_worker_image() {
    log "building Docker image for spur-context-worker (remote Docker build on VM)..."

    # Ensure ECR repo exists.
    aws ecr describe-repositories --repository-names "$WORKER_ECR_REPO" --region "$AWS_REGION_VAL" 2>/dev/null \
        || aws ecr create-repository --repository-name "$WORKER_ECR_REPO" --region "$AWS_REGION_VAL" >/dev/null

    local ecr_uri="${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION_VAL}.amazonaws.com/${WORKER_ECR_REPO}"
    local full_tag="${ecr_uri}:${WORKER_IMAGE_TAG}"

    # Build the Docker image entirely on the remote VM and push to ECR — no
    # local Docker or binary fetch needed. The VM has Docker installed via
    # startup-aws.sh and ECR push permissions via the spur-ecr-push IAM policy.
    # The --binary flag points at the worker binary already built on the VM's
    # target dir by build_worker() above.
    cd "$REPO_ROOT"
    scripts/cloud-build/docker-build.sh \
        --binary target/release/spur-context-worker \
        --dockerfile-inline 'FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git curl tar unzip ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
COPY spur-context-worker /usr/local/bin/spur-context-worker
ENTRYPOINT ["/usr/local/bin/spur-context-worker"]' \
        --tag "$full_tag"

    log "worker image pushed: $full_tag"
    echo "$full_tag"
}

main() {
    local zip_path="$REPO_ROOT/target/lambda/spur-context-service.zip"
    local local_zip=""
    local skip_worker=false
    local worker_image_uri=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --local-zip)   local_zip="$2"; shift 2 ;;
            --skip-worker) skip_worker=true; shift ;;
            *) break ;;
        esac
    done

    # Build + push worker container image (unless --skip-worker).
    if [[ "$skip_worker" == "false" ]]; then
        build_worker
        worker_image_uri="$(build_and_push_worker_image)"
    fi

    # Build or reuse Lambda zip.
    if [[ -n "$local_zip" ]]; then
        zip_path="$local_zip"
    elif [[ ! -f "$zip_path" ]]; then
        mkdir -p "$(dirname "$zip_path")"
        download_extensions
        build_binary
        package_zip "$zip_path"
    fi

    log "running terraform..."
    cd "$INFRA_DIR"
    terraform init -upgrade

    local tf_vars=(-var "lambda_zip_path=$zip_path")
    if [[ -n "$worker_image_uri" ]]; then
        tf_vars+=(-var "worker_ecr_image=$worker_image_uri")
    fi

    terraform apply "${tf_vars[@]}" -auto-approve

    log "deployed. API URL:"
    terraform output -raw api_url
    echo ""
}

main "$@"
