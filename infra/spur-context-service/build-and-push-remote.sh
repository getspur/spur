#!/bin/bash
# Build both spur CLI + spur-context-worker and push Docker image to ECR.
# Runs on the builder VM after the workspace is synced.
set -e

export HOME=${HOME:-/home/admin}
source /etc/profile.d/spur-build.sh 2>/dev/null || true

RUST_LLD=/mnt/cargo/rust-lld-driver/ld.lld
RUST_LLD_BIN=$(find ${RUSTUP_HOME:-/mnt/cargo/rustup}/toolchains -path "*/bin/rust-lld" -type f 2>/dev/null | sort | tail -1)
mkdir -p "$(dirname $RUST_LLD)"
ln -sfn "$RUST_LLD_BIN" "$RUST_LLD"

export TMPDIR=/mnt/cargo/tmp && mkdir -p $TMPDIR
export CARGO_BUILD_JOBS=16
export RUSTFLAGS="-Ctarget-cpu=neoverse-n1 -Ctarget-feature=+lse -Clinker=clang -Clink-arg=-fuse-ld=$RUST_LLD"
export CFLAGS="-mcpu=neoverse-n1 -O2"
export CXXFLAGS="-mcpu=neoverse-n1 -O2"
export AWS_REGION=${AWS_REGION:-ap-southeast-5}
ECR_TAG="065285885105.dkr.ecr.${AWS_REGION}.amazonaws.com/spur-context-worker:latest"

echo "=== [1/4] Building spur CLI (workspace, duckdb v1.4.4) ==="
cd ~/spur/main
cargo build -p spur-cli --release 2>&1 | tail -3
SPUR_BIN=/mnt/cargo/targets/spur/main/release/spur
ls -lh "$SPUR_BIN" || { echo "ERROR: spur CLI binary not found"; exit 1; }

echo "=== [2/4] Building spur-context-worker (standalone, duckdb v1.5.4) ==="
cd ~/spur/main/crates/spur-context-service
cargo build --features worker --release 2>&1 | tail -3
WORKER_BIN=target/release/spur-context-worker
ls -lh "$WORKER_BIN" || { echo "ERROR: worker binary not found"; exit 1; }

echo "=== [3/4] Building Docker image ==="
CTX=/tmp/docker-build.$$
mkdir -p "$CTX"
cp "$SPUR_BIN" "$CTX/spur"
cp "$WORKER_BIN" "$CTX/spur-context-worker"
# Write Dockerfile — includes DuckDB CLI v1.5.4 for the translate step
# (the Rust duckdb crate's linux_arm64 DuckLake extension has a bug where
# INSERTs don't flush to S3; the CLI binary works correctly)
cat > "$CTX/Dockerfile" <<'DOCKERFILE'
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git curl tar unzip ca-certificates && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL -o /tmp/duckdb.zip "https://github.com/duckdb/duckdb/releases/download/v1.5.4/duckdb_cli-linux-arm64.zip" && unzip -o /tmp/duckdb.zip -d /tmp && install -m 0755 /tmp/duckdb /usr/local/bin/duckdb && rm -f /tmp/duckdb.zip /tmp/duckdb
WORKDIR /workspace
COPY spur-context-worker /usr/local/bin/spur-context-worker
COPY spur /usr/local/bin/spur
ENTRYPOINT ["/usr/local/bin/spur-context-worker"]
DOCKERFILE

aws ecr get-login-password --region "$AWS_REGION" | docker login --username AWS --password-stdin "$(echo $ECR_TAG | sed 's|/.*||')"
cd "$CTX"
docker build -t "$ECR_TAG" .
rm -rf "$CTX"

echo "=== [4/4] Pushing to ECR ==="
docker push "$ECR_TAG"
echo "DOCKER_PUSH_OK $ECR_TAG"
