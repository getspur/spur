#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_spur_tui "help-overlay"
wait_text "No agents configured"
type_text "?"
wait_text "Dashboard — Modes"
expect_text "Dashboard — Navigation"
expect_text "Toggle verbose mode"
quit_cleanly
