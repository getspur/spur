#!/usr/bin/env bash
# Demo-local entrypoint → shared VHS/e2e TUI launcher.
# Keep tapes typed as `./bin/run-spur-tui.sh` (same pattern as scripts/e2e/vhs/).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$script_dir/../../../vhs/bin/run-spur-tui.sh" "$@"
