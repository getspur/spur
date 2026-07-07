#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

session_id="e2e-populated-session"
activity_anchor="e2e picker seeded prompt"
spur_bin="$(spur_e2e_resolve_spur_bin)"
export SPUR_BIN="$spur_bin"

open_isolated_shell_use_session "session-picker-populated"
seed_agent_model_catalog
touch "$SPUR_E2E_WORKSPACE/.spur/session-list-enabled"
run_su submit "$(shell_quote "$spur_bin") tui --dashboard"
wait_text "Type a task below"

session_day="$(date -u +%Y/%m/%d)"
history_dir="$SPUR_E2E_HOME/.codex/sessions/$session_day"
mkdir -p "$history_dir"
cat >"$history_dir/rollout-$session_id.jsonl" <<EOF
{"type":"event_msg","payload":{"type":"user_message","message":"$activity_anchor"}}
EOF

press_key Ctrl+K
wait_text "Go to"
type_text "Sessions"
wait_text "Sessions"
press_key Enter
wait_text "TODAY"
wait_text "$activity_anchor"
press_key Escape
wait_text "Type a task below"
quit_cleanly
