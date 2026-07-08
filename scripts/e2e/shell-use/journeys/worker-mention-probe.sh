#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

# Cold catalog: no seeding. The first mention must degrade gracefully,
# and the background probe (a real ACP handshake against the fixture
# worker) must unlock the model/effort slots for later mentions.
launch_spur_tui_no_catalog "worker-mention-probe"
wait_text "Type a task below"

# With no catalog entry, the agent slot is the final slot: accepting it
# atomizes immediately with neither model nor effort. The mention hint
# bar explains the degradation while the background probe runs.
type_text "@worker:codex"
wait_text "rust-reviewer"
expect_text "fetching codex models"
press_key Tab
wait_text "agent=rust-reviewer"

# Delete the atom; the mention marked the worker for a background
# catalog probe, which runs against the live fake worker.
press_key Backspace
wait_text "Enter to submit"

# Once the probe lands, the same cascade gains the model and effort
# slots.
wait_text "catalog ready"
cascade_until_model_slot
press_key Tab
wait_text "e2e deep reasoning"
press_key Tab
wait_text "model=gpt-5-codex"
expect_text "agent=rust-reviewer"
expect_text "effort=high"

press_key Backspace
wait_text "Enter to submit"
quit_cleanly
