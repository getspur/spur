#!/usr/bin/env bash
# Live UAT: focus composer and type a draft (does NOT send — no model spend).
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "composer-draft"
wait_text "Lineage"
wait_text "INSERT"
press_key Enter
sleep_ms 0.25
type_text "draft only - do not send (demo capture)"
expect_text "draft only"
# Esc / clear path: leave draft and quit without Enter-send
press_key Escape
sleep_ms 0.3
quit_live
