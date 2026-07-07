#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
e2e_root="$(cd "$script_dir/../.." && pwd)"
# shellcheck disable=SC1091
source "$e2e_root/lib/isolate.sh"
# shellcheck disable=SC1091
source "$e2e_root/lib/spur-bin.sh"

spur_bin="$(spur_e2e_resolve_spur_bin)"

# shellcheck disable=SC2329
cleanup() {
  spur_e2e_cleanup_isolation
}
trap cleanup EXIT

spur_e2e_isolate "spur-vhs" >/dev/null

export HOME="$SPUR_E2E_HOME"
export XDG_CONFIG_HOME="$SPUR_E2E_XDG_CONFIG_HOME"
export XDG_DATA_HOME="$SPUR_E2E_XDG_DATA_HOME"
export XDG_STATE_HOME="$SPUR_E2E_XDG_STATE_HOME"
export XDG_CACHE_HOME="$SPUR_E2E_XDG_CACHE_HOME"
export CI="$SPUR_E2E_CI"
export SPUR_NO_UPGRADE_CHECK="$SPUR_E2E_NO_UPGRADE_CHECK"
export SPUR_TUI_MOUSE_CAPTURE="$SPUR_E2E_TUI_MOUSE_CAPTURE"
export SPUR_LICENSE_TEST_STRIP_KEYS="$SPUR_E2E_LICENSE_TEST_STRIP_KEYS"

cd "$SPUR_E2E_WORKSPACE"
set +e
"$spur_bin" tui
status=$?
set -e

printf '\033[2J\033[H'
printf 'VHS_SPUR_EXITED status=%s\n' "$status"
exit "$status"
