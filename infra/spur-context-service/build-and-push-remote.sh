#!/usr/bin/env bash
# Compatibility wrapper for the canonical worker image path.
#
# Keep worker image assembly in deploy.sh so the image contents, Dockerfile,
# Graviton2-safe build guard, and smoke checks cannot drift from the documented
# deployment path.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec "$SCRIPT_DIR/deploy.sh" --worker-image-only "$@"
