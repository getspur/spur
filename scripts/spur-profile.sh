#!/usr/bin/env bash
set -euo pipefail

# SPUR Performance Profiling Helper
# Wraps `spur profile` for quick shell/CI access.
#
# Usage: ./scripts/spur-profile.sh [setup|flamegraph|bench|monitor]

COMMAND="${1:-flamegraph}"

case "$COMMAND" in
    setup)
        echo "[spur-profile] Running one-step profiling setup..."
        cargo run --bin spur -- profile setup
        ;;
    flamegraph)
        echo "[spur-profile] Generating flamegraph for spur tui..."
        cargo flamegraph --profile profiling --bin spur -- tui
        ;;
    bench)
        echo "[spur-profile] Running benchmarks with profiling profile..."
        cargo bench --profile profiling
        ;;
    monitor)
        echo "[spur-profile] Starting resource monitor..."
        cargo run --profile profiling --bin spur -- profile monitor
        ;;
    *)
        echo "Usage: $0 {setup|flamegraph|bench|monitor}"
        exit 1
        ;;
esac
