#!/usr/bin/env bash
# Shared helpers for worker-mention journeys. Not a journey itself —
# sourced by worker-mention-*.sh after lib.sh.
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

# start_spur_tui + catalog seeding between isolation and TUI launch.
# `--dashboard` pins the landing decision: with agents configured, bare
# `spur tui` spawns the default brain and lands in the session view,
# while these journeys drive the pre-session dashboard composer.
launch_spur_tui_with_catalog() {
  local journey="$1"
  local command spur_bin

  spur_bin="$(spur_e2e_resolve_spur_bin)"
  export SPUR_BIN="$spur_bin"

  open_isolated_shell_use_session "$journey"
  seed_agent_model_catalog
  command="$(shell_quote "$spur_bin") tui --dashboard"
  run_su submit "$command"
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
