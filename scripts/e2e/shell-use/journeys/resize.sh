#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

resize_terminal() {
  local resize_cols="$1"
  local resize_rows="$2"

  run_su resize "$resize_cols" "$resize_rows"
  run_su wait idle --timeout "$timeout_ms"
}

start_spur_tui "resize"
wait_text "No agents configured"
resize_terminal 100 30
expect_text "No agents configured"
resize_terminal 72 22
expect_text "No agents configured"
expect_text "Ctrl+K: go"
quit_cleanly
