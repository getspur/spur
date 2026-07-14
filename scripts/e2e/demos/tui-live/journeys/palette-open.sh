#!/usr/bin/env bash
# Live UAT: command palette on a real project with sessions/workers populated.
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "palette-open"
wait_text "Lineage"
press_key Ctrl+K
wait_text "Go to"
expect_text "esc dismiss"
press_key Escape
wait_text "Lineage"
quit_live
