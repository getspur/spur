#!/usr/bin/env bash
# Compatibility wrapper for the canonical worker image path.
#
# Keep worker image assembly in deploy.sh so the image contents, Dockerfile,
# Graviton2-safe build guard, and smoke checks cannot drift from the documented
# deployment path.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

SPUR_CONTEXT_SERVICE_BUILD_MODE="${SPUR_CONTEXT_SERVICE_BUILD_MODE:-remote}"
export SPUR_CONTEXT_SERVICE_BUILD_MODE

exec "$SCRIPT_DIR/deploy.sh" --worker-image-only "$@"
