#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_no_catalog "agent-config-open"
wait_text "Type a task below"

press_key Ctrl+K
type_text "/configure codex"
press_key Enter

wait_text "Settings: codex"
expect_text "codex"
expect_text "skip_permissions"
expect_text "Esc back"

press_key Escape
wait_text "Type a task below"
quit_cleanly
