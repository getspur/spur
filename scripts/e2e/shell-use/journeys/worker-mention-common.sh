#!/usr/bin/env bash
# Shared helpers for worker-mention journeys. Not a journey itself —
# sourced by worker-mention-*.sh after lib.sh.
# shellcheck disable=SC2154  # shell_use_bin/session_name are assigned by lib.sh.
#
# The fixture provides the worker config, the `.spur/agents/rust-reviewer.md`
# profile, and the fake ACP worker. The agent-model catalog is seeded here
# at runtime (probed_at must be fresh: the cache has a 24h TTL) so the
# model/effort slots are deterministic without a live probe.

seed_agent_model_catalog() {
  local cache_dir="$SPUR_E2E_HOME/.spur/cache"
  local probed_at

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
}

# start_spur_tui with optional catalog seeding between isolation and TUI
# launch. `--dashboard` pins the landing decision: with agents configured,
# bare `spur tui` spawns the default brain and lands in the session view,
# while these journeys drive the pre-session dashboard composer.
launch_spur_tui_worker_fixture() {
  local journey="$1"
  local seed="$2"
  local command spur_bin

  spur_bin="$(spur_e2e_resolve_spur_bin)"
  export SPUR_BIN="$spur_bin"

  open_isolated_shell_use_session "$journey"
  if [[ "$seed" == "seed-catalog" ]]; then
    seed_agent_model_catalog
  fi
  command="$(shell_quote "$spur_bin") tui --dashboard"
  run_su submit "$command"
}

launch_spur_tui_with_catalog() {
  launch_spur_tui_worker_fixture "$1" "seed-catalog"
}

# Cold-catalog variant for probe-lifecycle journeys: the fresh isolated
# HOME has no agent-model catalog, so the first worker mention has no
# model/effort slots until the background probe lands.
launch_spur_tui_no_catalog() {
  launch_spur_tui_worker_fixture "$1" "no-catalog"
}

# Retry the full cascade until the background-probed catalog exposes the
# model slot. There is no UI signal for probe completion, so early
# attempts may still see an agent-final cascade and atomize without
# model/effort — delete the atom and try again.
cascade_until_model_slot() {
  local attempt

  for attempt in 1 2 3 4 5; do
    type_text "@worker:codex"
    wait_text "rust-reviewer"
    press_key Tab
    if "$shell_use_bin" --session "$session_name" expect text "GPT-5 Codex" --no-strict --timeout 2000 >/dev/null 2>&1; then
      return 0
    fi
    # Probe not landed yet: the accept atomized early. Delete the atom
    # (protected range: one Backspace removes it whole) and retry.
    press_key Backspace
    wait_text "Enter to submit"
  done

  printf 'model slot never appeared after %s attempts\n' "$attempt" >&2
  dump_session >&2
  return 1
}

# Drive the cascading worker → agent → model → effort picker to a fully
# enriched atom. Each Tab accepts the highlighted slot candidate; the
# registry then opens the next non-empty slot.
compose_worker_mention_cascade() {
  type_text "@worker:codex"
  wait_text "rust-reviewer"
  # Popup width is container/2 (40 cols at 80): long descriptions are
  # truncated, so assert a prefix that always fits.
  expect_text "Reviews Rust diffs"
  press_key Tab
  wait_text "GPT-5 Codex"
  expect_text "e2e frontier model"
  press_key Tab
  wait_text "e2e deep reasoning"
  press_key Tab
  wait_text "model=gpt-5-codex"
  expect_text "agent=rust-reviewer"
  expect_text "effort=high"
}
