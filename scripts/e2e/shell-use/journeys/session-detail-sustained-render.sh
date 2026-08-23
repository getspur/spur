#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
: "${SHELL_USE_TIMEOUT_MS:=30000}"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

transcript_lines="${SPUR_E2E_TRANSCRIPT_LINES:-$((SPUR_E2E_ROWS * SPUR_E2E_ROWS))}"
render_cycles="${SPUR_E2E_RENDER_CYCLES:-$SPUR_E2E_ROWS}"
resize_delta="${SPUR_E2E_RESIZE_DELTA:-1}"

if [[ ! "$transcript_lines" =~ ^[1-9][0-9]*$ ]]; then
  printf 'SPUR_E2E_TRANSCRIPT_LINES must be a positive integer, got: %s\n' "$transcript_lines" >&2
  exit 2
fi
if [[ ! "$render_cycles" =~ ^[1-9][0-9]*$ ]]; then
  printf 'SPUR_E2E_RENDER_CYCLES must be a positive integer, got: %s\n' "$render_cycles" >&2
  exit 2
fi
if [[ ! "$resize_delta" =~ ^[0-9]+$ ]]; then
  printf 'SPUR_E2E_RESIZE_DELTA must be a non-negative integer, got: %s\n' "$resize_delta" >&2
  exit 2
fi

spur_bin="$(spur_e2e_resolve_spur_bin)"
export SPUR_BIN="$spur_bin"

open_isolated_shell_use_session "session-detail-sustained-render"
git -C "$SPUR_E2E_WORKSPACE" init -q
printf '%s\n' "$transcript_lines" >"$SPUR_E2E_WORKSPACE/.spur/sustained-render-lines"
seed_agent_model_catalog

command="$(shell_quote "$spur_bin") tui --dashboard"
run_su submit "$command"
wait_text "Type a task below"
compose_worker_mention_cascade

type_text " exercise sustained transcript rendering"
press_key Enter

completion_marker="sustained render complete: $transcript_lines lines"
wait_text "$completion_marker"

for ((cycle = 0; cycle < render_cycles; cycle++)); do
  press_key PageUp
  run_su resize "$((cols + resize_delta))" "$((rows + resize_delta))"
  press_key PageDown
  run_su resize "$cols" "$rows"
done

press_key G
wait_text "$completion_marker"
quit_cleanly
