#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_spur_tui "insights-open"
wait_text "No agents configured"
press_key Alt+a
run_su wait text "Analytics unavailable in this build|Refreshing\\.\\.\\." --regex --timeout "$timeout_ms"
press_key Escape
wait_text "No agents configured"
quit_cleanly
