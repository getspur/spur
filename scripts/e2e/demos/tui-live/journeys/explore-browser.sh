#!/usr/bin/env bash
# Live UAT: explore browser on real synced ecosystem catalog.
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "explore-browser"
wait_text "Lineage"
open_explore_browser
expect_text "Skills"
expect_text "Sources"
expect_text "pool"
press_key Escape
wait_text "Lineage"
quit_live
