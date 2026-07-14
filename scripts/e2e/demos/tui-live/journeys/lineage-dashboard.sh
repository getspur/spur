#!/usr/bin/env bash
# Live UAT: open real project TUI and see lineage / activity surface.
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "lineage-dashboard"
wait_text "Lineage"
expect_text "INSERT"
# Soft: either live brains or a quieter project still has the lineage chrome.
expect_text "Activity"
quit_live
