#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_with_catalog "worker-mention-slots"
wait_text "Type a task below"

# Double comma explicitly skips the agent slot: the model slot opens
# directly with catalog candidates.
type_text "@worker:codex,,"
wait_text "GPT-5 Codex"
expect_text "e2e frontier model"
press_key Tab
wait_text "e2e deep reasoning"
press_key Tab

# The atom composes without an agent: `model=` follows the worker name
# directly (an accepted agent would sit between them).
wait_text "codex model=gpt-5-codex"
expect_text "effort=high"

# Mention atoms are protected ranges: a single Backspace deletes the
# whole atom, and the empty-compose hint proves the input bar emptied.
press_key Backspace
wait_text "Enter to submit"

# Ambiguous slot text is a filter, not an auto-pick: both rust-*
# profiles stay visible instead of one being silently committed.
type_text "@worker:codex,rust"
wait_text "rust-tester"
expect_text "rust-reviewer"

# Refining to a unique high-confidence prefix auto-advances: "rust-rev"
# matches only rust-reviewer, so the model slot opens without a Tab.
type_text "-rev"
wait_text "GPT-5 Codex"
press_key Tab
wait_text "e2e deep reasoning"
press_key Tab
wait_text "agent=rust-reviewer"
expect_text "model=gpt-5-codex"

press_key Backspace
wait_text "Enter to submit"
quit_cleanly
