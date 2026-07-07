#!/usr/bin/env bash
set -euo pipefail

e2e_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$e2e_dir" rev-parse --show-toplevel)"

cols=80
rows=24
timeout_ms="${SHELL_USE_TIMEOUT_MS:-10000}"
shell_use_bin="${SHELL_USE_BIN:-"$("$e2e_dir/install.sh")"}"
spur_bin="${SPUR_BIN:-"$repo_root/target/debug/spur"}"

if [[ ! -x "$shell_use_bin" ]]; then
  printf 'shell-use binary is not executable: %s\n' "$shell_use_bin" >&2
  exit 2
fi

if [[ ! -x "$spur_bin" ]]; then
  printf 'spur binary is not executable: %s\n' "$spur_bin" >&2
  printf 'Build it with: SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli\n' >&2
  exit 2
fi

session_name=""
work_root=""

cleanup_shell_use_session() {
  local status=$?
  if [[ -n "${session_name:-}" ]]; then
    "$shell_use_bin" --session "$session_name" close >/dev/null 2>&1 || true
  fi
  if [[ -n "${work_root:-}" ]]; then
    rm -rf "$work_root"
  fi
  exit "$status"
}

trap cleanup_shell_use_session EXIT

dump_session() {
  if [[ -z "${session_name:-}" ]]; then
    return
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
  local workspace home xdg_config xdg_data xdg_state xdg_cache command

  work_root="$(mktemp -d "${TMPDIR:-/tmp}/spur-shell-use.XXXXXX")"
  workspace="$work_root/workspace"
  home="$work_root/home"
  xdg_config="$work_root/xdg-config"
  xdg_data="$work_root/xdg-data"
  xdg_state="$work_root/xdg-state"
  xdg_cache="$work_root/xdg-cache"

  mkdir -p \
    "$workspace/.spur" \
    "$home/.spur" \
    "$xdg_config" \
    "$xdg_data" \
    "$xdg_state" \
    "$xdg_cache"

  printf '%s\n' '{"version":1,"first_run_at":"2026-07-07T00:00:00Z"}' > "$home/.spur/onboarded"

  session_name="spur-shell-use-${RUN_INDEX:-1}-${journey}-$$"
  run_su open \
    --shell bash \
    --cols "$cols" \
    --rows "$rows" \
    --cwd "$workspace" \
    --env "HOME=$home" \
    --env "XDG_CONFIG_HOME=$xdg_config" \
    --env "XDG_DATA_HOME=$xdg_data" \
    --env "XDG_STATE_HOME=$xdg_state" \
    --env "XDG_CACHE_HOME=$xdg_cache" \
    --env "CI=false" \
    --env "SPUR_NO_UPGRADE_CHECK=1" \
    --env "SPUR_TUI_MOUSE_CAPTURE=0" \
    --env "SPUR_LICENSE_TEST_STRIP_KEYS="

  command="$(shell_quote "$spur_bin") tui"
  run_su submit "$command"
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
