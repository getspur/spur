#!/usr/bin/env bash
set -euo pipefail

: "${SPUR_E2E_COLS:=80}"
: "${SPUR_E2E_ROWS:=24}"
: "${SPUR_E2E_FIXTURE:=no-agents}"

spur_e2e_isolate_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spur_e2e_isolate_repo_root="$(git -C "$spur_e2e_isolate_lib_dir" rev-parse --show-toplevel)"

spur_e2e_copy_fixture() {
  local fixture_dir="$1"
  local workspace="$2"
  local had_dotglob=0
  local had_nullglob=0
  local entries=()
  local entry

  shopt -q dotglob && had_dotglob=1 || true
  shopt -q nullglob && had_nullglob=1 || true
  shopt -s dotglob nullglob
  entries=("$fixture_dir"/*)
  if [[ "$had_dotglob" -eq 0 ]]; then
    shopt -u dotglob
  fi
  if [[ "$had_nullglob" -eq 0 ]]; then
    shopt -u nullglob
  fi

  for entry in "${entries[@]}"; do
    if [[ "$(basename "$entry")" == ".gitkeep" ]]; then
      continue
    fi
    cp -a "$entry" "$workspace/"
  done
}

spur_e2e_isolate() {
  local prefix="${1:-spur-e2e}"
  local fixture="${SPUR_E2E_FIXTURE:-no-agents}"
  local fixture_dir="$spur_e2e_isolate_repo_root/scripts/e2e/fixtures/$fixture"
  local root workspace home xdg_config xdg_data xdg_state xdg_cache

  if [[ ! -d "$fixture_dir" ]]; then
    printf 'error: SPUR_E2E_FIXTURE=%s not found at %s\n' "$fixture" "$fixture_dir" >&2
    return 2
  fi

  root="$(mktemp -d "${TMPDIR:-/tmp}/${prefix}.XXXXXX")"
  workspace="$root/workspace"
  home="$root/home"
  xdg_config="$root/xdg-config"
  xdg_data="$root/xdg-data"
  xdg_state="$root/xdg-state"
  xdg_cache="$root/xdg-cache"

  mkdir -p \
    "$workspace/.spur" \
    "$home/.spur" \
    "$xdg_config" \
    "$xdg_data" \
    "$xdg_state" \
    "$xdg_cache"

  printf '%s\n' '{"version":1,"first_run_at":"2026-07-07T00:00:00Z"}' >"$home/.spur/onboarded"
  spur_e2e_copy_fixture "$fixture_dir" "$workspace"

  export SPUR_E2E_WORK_ROOT="$root"
  export SPUR_E2E_WORKSPACE="$workspace"
  export SPUR_E2E_HOME="$home"
  export SPUR_E2E_XDG_CONFIG_HOME="$xdg_config"
  export SPUR_E2E_XDG_DATA_HOME="$xdg_data"
  export SPUR_E2E_XDG_STATE_HOME="$xdg_state"
  export SPUR_E2E_XDG_CACHE_HOME="$xdg_cache"
  export SPUR_E2E_CI=false
  export SPUR_E2E_NO_UPGRADE_CHECK=1
  export SPUR_E2E_TUI_MOUSE_CAPTURE=0
  export SPUR_E2E_LICENSE_TEST_STRIP_KEYS=""
  export SPUR_E2E_FIXTURE_ACTIVE="$fixture"

  printf '%s\n' "$workspace"
}

spur_e2e_shell_use_env_args() {
  printf '%s\0' \
    --env "HOME=$SPUR_E2E_HOME" \
    --env "XDG_CONFIG_HOME=$SPUR_E2E_XDG_CONFIG_HOME" \
    --env "XDG_DATA_HOME=$SPUR_E2E_XDG_DATA_HOME" \
    --env "XDG_STATE_HOME=$SPUR_E2E_XDG_STATE_HOME" \
    --env "XDG_CACHE_HOME=$SPUR_E2E_XDG_CACHE_HOME" \
    --env "CI=$SPUR_E2E_CI" \
    --env "SPUR_NO_UPGRADE_CHECK=$SPUR_E2E_NO_UPGRADE_CHECK" \
    --env "SPUR_TUI_MOUSE_CAPTURE=$SPUR_E2E_TUI_MOUSE_CAPTURE" \
    --env "SPUR_LICENSE_TEST_STRIP_KEYS=$SPUR_E2E_LICENSE_TEST_STRIP_KEYS"
}

spur_e2e_cleanup_isolation() {
  if [[ -n "${SPUR_E2E_WORK_ROOT:-}" ]]; then
    rm -rf "$SPUR_E2E_WORK_ROOT"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  spur_e2e_isolate "${1:-spur-e2e}"
fi
