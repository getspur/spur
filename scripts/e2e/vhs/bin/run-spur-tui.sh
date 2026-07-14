#!/usr/bin/env bash
# Shared SPUR TUI launcher for VHS goldens and demos/tui-journeys media.
#
# Env (all optional; defaults preserve existing visual goldens):
#   SPUR_E2E_FIXTURE       fixture under scripts/e2e/fixtures/ (default: no-agents)
#   SPUR_E2E_TUI_ARGS      extra args after `tui` (e.g. "--dashboard")
#   SPUR_E2E_SEED_CATALOG  if 1, seed a fresh agent-model catalog in HOME
#                          (worker-mentions journeys / Arc B demos)
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

if [[ "${SPUR_E2E_SEED_CATALOG:-0}" == "1" ]]; then
  cache_dir="$SPUR_E2E_HOME/.spur/cache"
  probed_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  mkdir -p "$cache_dir"
  cat >"$cache_dir/agent-model-catalog.json" <<EOF
{
  "version": 1,
  "entries": {
    "codex": {
      "probed_at": "$probed_at",
      "cli_identity": "bash .spur/fake-worker.sh",
      "models": [
        {"value": "gpt-5-codex", "name": "GPT-5 Codex", "description": "e2e frontier model"}
      ],
      "efforts": [
        {"value": "high", "name": "High", "description": "e2e deep reasoning"}
      ]
    }
  }
}
EOF
fi

export HOME="$SPUR_E2E_HOME"
export XDG_CONFIG_HOME="$SPUR_E2E_XDG_CONFIG_HOME"
export XDG_DATA_HOME="$SPUR_E2E_XDG_DATA_HOME"
export XDG_STATE_HOME="$SPUR_E2E_XDG_STATE_HOME"
export XDG_CACHE_HOME="$SPUR_E2E_XDG_CACHE_HOME"
export CI="$SPUR_E2E_CI"
export SPUR_NO_UPGRADE_CHECK="$SPUR_E2E_NO_UPGRADE_CHECK"
export SPUR_TUI_MOUSE_CAPTURE="$SPUR_E2E_TUI_MOUSE_CAPTURE"
export SPUR_LICENSE_TEST_STRIP_KEYS="$SPUR_E2E_LICENSE_TEST_STRIP_KEYS"

# Optional extra args after `tui` (empty default = bare `spur tui`).
# Avoid "${empty_array[@]}" under `set -u` (bash 3.2 unbound error).
cd "$SPUR_E2E_WORKSPACE"
set +e
if [[ -n "${SPUR_E2E_TUI_ARGS:-}" ]]; then
  # shellcheck disable=SC2086
  "$spur_bin" tui $SPUR_E2E_TUI_ARGS
else
  "$spur_bin" tui
fi
status=$?
set -e

printf '\033[2J\033[H'
printf 'VHS_SPUR_EXITED status=%s\n' "$status"
exit "$status"
