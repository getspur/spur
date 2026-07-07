#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_spur_tui "session-picker-open"
wait_text "No agents configured"
press_key Ctrl+K
wait_text "Go to"
type_text "Sessions"
wait_text "Sessions"
press_key Enter
wait_text "Sessions"
press_key Escape
wait_text "No agents configured"
quit_cleanly
