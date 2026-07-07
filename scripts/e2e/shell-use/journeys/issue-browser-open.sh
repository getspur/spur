#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_spur_tui "issue-browser-open"
wait_text "No agents configured"
press_key Ctrl+K
wait_text "Go to"
type_text "Issues"
press_key Enter
wait_text "No issue tracker configured"
press_key Escape
wait_text "No agents configured"
quit_cleanly
