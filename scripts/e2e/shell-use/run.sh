#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"

shell_use_bin="${SHELL_USE_BIN:-"$("$script_dir/install.sh")"}"
spur_bin="${SPUR_BIN:-"$repo_root/target/debug/spur"}"
runs="${SHELL_USE_RUNS:-1}"

if [[ ! "$runs" =~ ^[0-9]+$ || "$runs" -lt 1 ]]; then
  printf 'SHELL_USE_RUNS must be a positive integer, got: %s\n' "$runs" >&2
  exit 2
fi

if [[ ! -x "$spur_bin" ]]; then
  printf 'spur binary is not executable: %s\n' "$spur_bin" >&2
  printf 'Build it with: SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli\n' >&2
  exit 2
fi

journeys=(
  "cold-launch"
  "help-overlay"
  "clean-quit"
)

printf 'shell-use: %s\n' "$shell_use_bin"
printf 'spur:      %s\n' "$spur_bin"
printf 'runs:      %s\n' "$runs"

failures=0
for ((run = 1; run <= runs; run++)); do
  for journey in "${journeys[@]}"; do
    printf '\n=== run %d/%d: %s ===\n' "$run" "$runs" "$journey"
    if RUN_INDEX="$run" SHELL_USE_BIN="$shell_use_bin" SPUR_BIN="$spur_bin" "$script_dir/journeys/$journey.sh"; then
      printf 'PASS run %d/%d %s\n' "$run" "$runs" "$journey"
    else
      status=$?
      printf 'FAIL run %d/%d %s (exit %d)\n' "$run" "$runs" "$journey" "$status"
      failures=$((failures + 1))
    fi
  done
done

printf '\n=== summary ===\n'
printf 'journeys: %d\n' "${#journeys[@]}"
printf 'runs:     %s\n' "$runs"
printf 'failures: %d\n' "$failures"

if [[ "$failures" -ne 0 ]]; then
  exit 1
fi
