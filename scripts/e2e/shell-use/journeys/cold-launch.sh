#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$journey_dir/../lib.sh"

start_spur_tui "cold-launch"
wait_text "No agents configured"
expect_text "SPUR"
expect_text "spur init"
quit_cleanly
