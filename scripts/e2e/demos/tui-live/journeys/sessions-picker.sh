#!/usr/bin/env bash
# Live UAT: open sessions picker on a real project (expects session history).
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "sessions-picker"
wait_text "Lineage"
press_key s
wait_text "Sessions"
expect_text "TODAY"
# Escape or leave picker then quit
press_key Escape
sleep 0.5
quit_live
