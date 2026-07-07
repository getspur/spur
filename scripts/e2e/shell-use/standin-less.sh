#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${SHELL_USE_TIMEOUT_MS:=5000}"
# shellcheck disable=SC1091
source "$script_dir/lib.sh"

open_isolated_shell_use_session "standin-less"

printf '%s\n' \
  "shell-use stand-in ready" \
  "This less session is a bounded full-screen TUI probe." \
  > "$SPUR_E2E_WORKSPACE/input.txt"

run_su submit "less input.txt"
wait_text "shell-use stand-in ready"
expect_text "shell-use stand-in ready"
press_key q
wait_command_done
expect_exit_code 0
