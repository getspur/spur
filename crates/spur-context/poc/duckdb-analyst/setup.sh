#!/usr/bin/env bash
# Thin shim — analyst DB build now lives in `spur-cli analyst build`.
# Kept for the documented entry point and CI usage. Forwards all flags.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

exec scripts/spur-cargo run --quiet -p spur-cli -- analyst build "$@"
