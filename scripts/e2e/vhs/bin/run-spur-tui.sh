#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${SPUR_BIN:-}" ]]; then
  echo "error: SPUR_BIN must point at the spur binary" >&2
  exit 2
fi

workspace="$(mktemp -d "${TMPDIR:-/tmp}/spur-vhs.XXXXXX")"
cleanup() {
  rm -rf "$workspace"
}
trap cleanup EXIT

repo="${workspace}/repo"
home="${workspace}/home"
xdg_config="${workspace}/xdg-config"
xdg_data="${workspace}/xdg-data"
xdg_state="${workspace}/xdg-state"
xdg_cache="${workspace}/xdg-cache"

mkdir -p \
  "${repo}/.spur" \
  "${home}/.spur" \
  "$xdg_config" \
  "$xdg_data" \
  "$xdg_state" \
  "$xdg_cache"

printf '%s\n' '{"version":1,"first_run_at":"2026-07-07T00:00:00Z"}' > "${home}/.spur/onboarded"

export HOME="$home"
export XDG_CONFIG_HOME="$xdg_config"
export XDG_DATA_HOME="$xdg_data"
export XDG_STATE_HOME="$xdg_state"
export XDG_CACHE_HOME="$xdg_cache"
export CI=false
export SPUR_NO_UPGRADE_CHECK=1
export SPUR_TUI_MOUSE_CAPTURE=0
export SPUR_LICENSE_TEST_STRIP_KEYS=""

cd "$repo"
set +e
"$SPUR_BIN" tui
status=$?
set -e

printf '\033[2J\033[H'
printf 'VHS_SPUR_EXITED status=%s\n' "$status"
exit "$status"
