#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$journey_dir/../lib.sh"

start_spur_tui "clean-quit"
wait_text "No agents configured"
press_key Ctrl+C
wait_text "Quit spur?"
press_key y
wait_command_done
expect_exit_code 0
