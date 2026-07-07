#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
shell_use_bin="${SHELL_USE_BIN:-"$("$script_dir/install.sh")"}"
timeout_ms="${SHELL_USE_TIMEOUT_MS:-5000}"
session_name="spur-shell-use-standin-less-$$"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spur-shell-use-less.XXXXXX")"

cleanup() {
  local status=$?
  "$shell_use_bin" --session "$session_name" close >/dev/null 2>&1 || true
  rm -rf "$tmp_dir"
  exit "$status"
}

trap cleanup EXIT

dump_session() {
  printf -- '--- shell-use state ---\n'
  "$shell_use_bin" --session "$session_name" state || true
  printf -- '--- shell-use text --full ---\n'
  "$shell_use_bin" --session "$session_name" text --full || true
  printf -- '--- end diagnostics ---\n'
}

run_su() {
  local output status

  printf '+ shell-use --session %s' "$session_name"
  printf ' %q' "$@"
  printf '\n'

  set +e
  output="$("$shell_use_bin" --session "$session_name" "$@" 2>&1)"
  status=$?
  set -e

  if [[ -n "$output" ]]; then
    printf '%s\n' "$output"
  fi

  if [[ "$status" -ne 0 ]]; then
    printf 'shell-use command failed with exit %s\n' "$status" >&2
    dump_session >&2
    return "$status"
  fi
}

printf '%s\n' \
  "shell-use stand-in ready" \
  "This less session is a bounded full-screen TUI probe." \
  > "$tmp_dir/input.txt"

run_su open --shell bash --cols 80 --rows 24 --cwd "$tmp_dir"
run_su submit "less input.txt"
run_su wait text "shell-use stand-in ready" --timeout "$timeout_ms"
run_su expect text "shell-use stand-in ready" --no-strict --timeout "$timeout_ms"
run_su press q
run_su wait command --timeout "$timeout_ms"
run_su expect exit-code 0
