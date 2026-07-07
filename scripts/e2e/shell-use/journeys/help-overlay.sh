#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$journey_dir/../lib.sh"

start_spur_tui "help-overlay"
wait_text "No agents configured"
type_text "?"
wait_text "Keyboard environment"
expect_text "Ctrl-C"
expect_text "Press ? or Esc to close"
quit_cleanly
