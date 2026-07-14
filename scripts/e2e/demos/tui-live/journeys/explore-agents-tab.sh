#!/usr/bin/env bash
# Live UAT: explore browser → Agents tab (synced personas).
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "explore-agents-tab"
wait_text "Lineage"
open_explore_browser
# Skills is default; Tab → Agents
press_key Tab
sleep_ms 0.4
# Agents tab labels the middle pane / header
expect_text "Agents"
# Soft: catalog still present
expect_text "catalog"
press_key Escape
wait_text "Lineage"
quit_live
