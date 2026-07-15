#!/usr/bin/env bash
# Live UAT: open sessions picker on a real project (expects session history).
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "sessions-picker"
story_session_land "Session Detail is ready before session navigation"
open_sessions_picker
story_soft_proof \
  "Existing session history is visible" \
  "TODAY" 1500 1.5 \
  "the empty picker remains a valid first-run state"
# Escape or leave picker then quit
press_key Escape
sleep 0.5
quit_live
