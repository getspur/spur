#!/usr/bin/env bash
set -euo pipefail

e2e_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
e2e_root="$(cd "$e2e_dir/.." && pwd)"
# shellcheck disable=SC1091
source "$e2e_root/lib/isolate.sh"
# shellcheck disable=SC1091
source "$e2e_root/lib/spur-bin.sh"

cols="$SPUR_E2E_COLS"
rows="$SPUR_E2E_ROWS"
timeout_ms="${SHELL_USE_TIMEOUT_MS:-10000}"
shell_use_bin="${SHELL_USE_BIN:-"$("$e2e_dir/install.sh")"}"

if [[ ! -x "$shell_use_bin" ]]; then
  printf 'shell-use binary is not executable: %s\n' "$shell_use_bin" >&2
  exit 2
fi

session_name=""

cleanup_shell_use_session() {
  local status=$?
  if [[ -n "${session_name:-}" ]]; then
    "$shell_use_bin" --session "$session_name" close >/dev/null 2>&1 || true
  fi
  spur_e2e_cleanup_isolation
  exit "$status"
}

trap cleanup_shell_use_session EXIT

dump_session() {
  local artifact_dir

  if [[ -z "${session_name:-}" ]]; then
    return
  fi

  if [[ -n "${SPUR_E2E_ARTIFACTS_DIR:-}" ]]; then
    artifact_dir="$SPUR_E2E_ARTIFACTS_DIR/shell-use/$session_name"
    mkdir -p "$artifact_dir"
    "$shell_use_bin" --session "$session_name" state >"$artifact_dir/state.txt" 2>&1 || true
    "$shell_use_bin" --session "$session_name" text --full >"$artifact_dir/text-full.txt" 2>&1 || true
  fi

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

shell_quote() {
  printf '%q' "$1"
}

start_spur_tui() {
  local journey="$1"
  local command spur_bin

  spur_bin="$(spur_e2e_resolve_spur_bin)"
  export SPUR_BIN="$spur_bin"

  open_isolated_shell_use_session "$journey"
  command="$(shell_quote "$spur_bin") tui"
  run_su submit "$command"
}

open_isolated_shell_use_session() {
  local journey="$1"
  local arg
  local env_args=()

  spur_e2e_isolate "spur-shell-use" >/dev/null
  while IFS= read -r -d '' arg; do
    env_args+=("$arg")
  done < <(spur_e2e_shell_use_env_args)

  session_name="spur-shell-use-${RUN_INDEX:-1}-${journey}-$$"
  run_su open \
    --shell bash \
    --cols "$cols" \
    --rows "$rows" \
    --cwd "$SPUR_E2E_WORKSPACE" \
    "${env_args[@]}"
}

wait_text() {
  run_su wait text "$1" --timeout "$timeout_ms"
}

expect_text() {
  run_su expect text "$1" --no-strict --timeout "$timeout_ms"
}

type_text() {
  run_su type "$1"
}

press_key() {
  run_su press "$@"
}

wait_command_done() {
  run_su wait command --timeout "$timeout_ms"
}

expect_exit_code() {
  run_su expect exit-code "$1"
}

quit_cleanly() {
  press_key Ctrl+C
  wait_text "Quit spur?"
  press_key y
  wait_command_done
  expect_exit_code 0
}
