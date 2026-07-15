#!/usr/bin/env bash
# Live UAT: command palette on a real project with sessions/workers populated.
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "palette-open"
story_session_land "Session Detail is ready before opening Go to"
press_key Ctrl+K
wait_text "Go to"
expect_text "esc dismiss"
press_key Escape
return_to_session_detail
story_session_land "Dismissing Go to returns to Session Detail"
quit_live
