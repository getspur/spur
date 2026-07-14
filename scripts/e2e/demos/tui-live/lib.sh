#!/usr/bin/env bash
# shell-use helpers for *live* project demos (no fixture isolation / no rm).
set -euo pipefail

demo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
e2e_root="$(cd "$demo_dir/../.." && pwd)"
shell_use_dir="$e2e_root/shell-use"
# shellcheck disable=SC1091
source "$e2e_root/lib/spur-bin.sh"

cols="${SPUR_DEMO_COLS:-120}"
rows="${SPUR_DEMO_ROWS:-36}"
timeout_ms="${SHELL_USE_TIMEOUT_MS:-15000}"
shell_use_bin="${SHELL_USE_BIN:-"$("$shell_use_dir/install.sh")"}"

if [[ ! -x "$shell_use_bin" ]]; then
  printf 'shell-use binary is not executable: %s\n' "$shell_use_bin" >&2
  exit 2
fi

if [[ -n "${SPUR_DEMO_PROJECT:-}" ]]; then
  project="$SPUR_DEMO_PROJECT"
else
  project="$(git -C "$e2e_root/../.." rev-parse --show-toplevel)"
fi

if [[ ! -d "$project/.spur" ]]; then
  printf 'error: not a SPUR project (missing .spur/): %s\n' "$project" >&2
  exit 2
fi

session_name=""

cleanup_live_session() {
  local status=$?
  if [[ -n "${session_name:-}" ]]; then
    "$shell_use_bin" --session "$session_name" close >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup_live_session EXIT

run_su() {
  local output status
  printf '+ shell-use --session %s' "$session_name"
  printf ' %q' "$@"
  printf '\n'
  set +e
  output="$("$shell_use_bin" --session "$session_name" "$@" 2>&1)"
  status=$?
  set -e
  if [[ -n "$output" ]]; then
    printf '%s\n' "$output"
  fi
  if [[ "$status" -ne 0 ]]; then
    printf 'shell-use command failed with exit %s\n' "$status" >&2
    "$shell_use_bin" --session "$session_name" text --full >&2 || true
    return "$status"
  fi
}

start_live_tui() {
  local journey="$1"
  local spur_bin command

  spur_bin="$(spur_e2e_resolve_spur_bin)"
  export SPUR_BIN="$spur_bin"

  session_name="spur-live-${journey}-$$"
  run_su open \
    --shell bash \
    --cols "$cols" \
    --rows "$rows" \
    --cwd "$project" \
    --env "SPUR_NO_UPGRADE_CHECK=1" \
    --env "SPUR_TUI_MOUSE_CAPTURE=0"

  # Default: dashboard landing (still shows live lineage when project has active work).
  command="$(printf '%q' "$spur_bin") tui --dashboard"
  run_su submit "$command"
}

wait_text() {
  run_su wait text "$1" --timeout "$timeout_ms"
}

expect_text() {
  run_su expect text "$1" --no-strict --timeout "$timeout_ms"
}

press_key() {
  run_su press "$@"
}

type_text() {
  run_su type -- "$1"
}

sleep_ms() {
  # Portable fractional sleep (seconds as float string ok for sleep).
  sleep "${1:-0.5}"
}

require_agent_send_opt_in() {
  if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" != "1" ]]; then
    cat >&2 <<'EOF'
error: agent-send journey is opt-in (real model spend).

  SPUR_DEMO_ALLOW_AGENT_SEND=1 ./uat.sh --mode uat
  # or only the send journey:
  SPUR_DEMO_ALLOW_AGENT_SEND=1 bash journeys/agent-send.sh
EOF
    return 2
  fi
}

# Leave composer / overlays. Session detail Esc = back; repeat to dashboard.
# Soft expects (no run_su dump on miss).
return_to_dashboard() {
  local i
  for i in 1 2 3 4 5; do
    set +e
    "$shell_use_bin" --session "$session_name" expect text "Lineage" --no-strict --timeout 500 >/dev/null 2>&1
    local rc=$?
    set -e
    if [[ "$rc" -eq 0 ]]; then
      return 0
    fi
    press_key Escape
    sleep_ms 0.3
  done
  return 0
}

blur_composer() {
  return_to_dashboard
}

# Soft text check without dump-on-fail noise.
soft_has_text() {
  set +e
  "$shell_use_bin" --session "$session_name" expect text "$1" --no-strict --timeout "${2:-1500}" >/dev/null 2>&1
  local rc=$?
  set -e
  return "$rc"
}

# Open sessions picker via palette (robust vs INSERT key capture).
open_sessions_picker() {
  return_to_dashboard
  sleep_ms 0.2
  # If still not on Lineage, Esc once more then open palette anyway.
  press_key Ctrl+K
  if ! soft_has_text "Go to" 4000; then
    press_key Escape
    sleep_ms 0.3
    press_key Ctrl+K
    wait_text "Go to"
  fi
  type_text "Sessions"
  sleep_ms 0.35
  press_key Enter
  wait_text "Sessions"
}

# Returns 0 if screen shows attached session detail.
_live_wait_session_detail() {
  local ms="${1:-6000}"
  set +e
  run_su wait text "Session ·" --timeout "$ms"
  local rc=$?
  set -e
  return "$rc"
}

_live_on_attach_conflict() {
  soft_has_text "another window" 1200
}

# Unsent draft dialog: "save and resume …? [y/n]"
_live_confirm_draft_if_needed() {
  if soft_has_text "unsent draft" 800 || soft_has_text "y/Enter confirm" 400; then
    printf '+ confirm unsent draft (y)\n'
    type_text "y"
    sleep_ms 0.8
    return 0
  fi
  return 1
}

# After Enter on a session row: handle draft dialog, attach conflict, or success.
_live_complete_session_attach() {
  local ms="${1:-7000}"
  sleep_ms 0.5
  _live_confirm_draft_if_needed || true
  if _live_wait_session_detail "$ms"; then
    expect_text "INSERT"
    return 0
  fi
  if _live_on_attach_conflict; then
    printf '+ attach conflict — new session (n)\n'
    type_text "n"
    sleep_ms 1.2
    if _live_wait_session_detail 10000; then
      expect_text "INSERT"
      return 0
    fi
    return 1
  fi
  # Draft dialog may reappear after conflict path
  _live_confirm_draft_if_needed || true
  if _live_wait_session_detail 5000; then
    expect_text "INSERT"
    return 0
  fi
  return 1
}

resume_session_skip_held() {
  # Walk the sessions list until attach succeeds. Top rows are often held by
  # other TUI windows ("Session attached in another window").
  open_sessions_picker
  sleep_ms 0.6
  # Start from the 3rd TODAY row (skip the two most-recent, often held/busy).
  press_key Down
  sleep_ms 0.25
  press_key Down
  sleep_ms 0.25

  local attempt=0
  while [[ "$attempt" -lt 8 ]]; do
    attempt=$((attempt + 1))
    printf '+ resume attempt %s\n' "$attempt"
    press_key Enter
    if _live_complete_session_attach 7000; then
      return 0
    fi
    # Still on picker / dialog — advance
    if soft_has_text "Sessions" 800; then
      press_key Down
      sleep_ms 0.35
    else
      # Escape draft dialog and re-open picker
      press_key Escape
      sleep_ms 0.4
      open_sessions_picker
      press_key Down
      sleep_ms 0.3
    fi
  done

  printf 'error: could not attach a free session after several attempts\n' >&2
  return 1
}

# For agent-send: attach any free session, or fall back to [N] new session
# so a real model turn can still be captured when the list is fully held.
attach_session_for_send() {
  if resume_session_skip_held; then
    return 0
  fi
  printf '+ resume exhausted — open new session for send (N)\n'
  # May still be on conflict dialog or picker
  set +e
  run_su expect text "another window" --no-strict --timeout 800
  local on_conflict=$?
  set -e
  if [[ "$on_conflict" -eq 0 ]]; then
    type_text "n"
  else
    open_sessions_picker
    sleep_ms 0.4
    # First row under Search is often "+ Start new session"
    press_key Enter
  fi
  sleep_ms 1
  if ! _live_wait_session_detail 15000; then
    wait_text "INSERT"
  fi
  expect_text "INSERT"
}

# Open a palette view by title substring (Plan, Issues, Explore, Sessions, …).
open_palette_view() {
  local title="$1"
  return_to_dashboard
  sleep_ms 0.2
  press_key Ctrl+K
  if ! soft_has_text "Go to" 4000; then
    press_key Escape
    sleep_ms 0.3
    press_key Ctrl+K
    wait_text "Go to"
  fi
  type_text "$title"
  sleep_ms 0.35
  press_key Enter
}

open_explore_browser() {
  open_palette_view "Explore"
  wait_text "Explore"
  expect_text "synced"
  expect_text "catalog"
}

# Enter Dashboard Navigate mode (Esc exits Compose so j/k hit the view).
enter_navigate_mode() {
  press_key Escape
  sleep_ms 0.35
  # Soft proof: footer often shows [NAV] when a panel owns keys
  set +e
  "$shell_use_bin" --session "$session_name" expect text "[NAV]" --no-strict --timeout 1500 >/dev/null 2>&1
  set -e
}

# Focus Agents (lineage tree) panel. Prefer Tab cycle (view-owned always);
# digit '1' is NOT a view-action char and would re-enter Compose.
focus_agents_panel() {
  enter_navigate_mode
  # Default focus may be Log; Tab toggles Agents ↔ Log.
  press_key Tab
  sleep_ms 0.4
  if ! soft_has_text "[Agents]" 1200; then
    press_key Tab
    sleep_ms 0.4
  fi
  soft_has_text "[Agents]" 2000 || soft_has_text "Lineage" 1000 || true
}

# Focus Activity log panel.
focus_log_panel() {
  enter_navigate_mode
  press_key Tab
  sleep_ms 0.35
  if ! soft_has_text "[Log]" 1000; then
    press_key Tab
    sleep_ms 0.35
  fi
}

# Walk lineage: select nodes, open detail (stream/artifacts/attempts/task/review).
# Proves brain↔worker tree is navigable and outputs are inspectable.
navigate_lineage_brain_and_workers() {
  focus_agents_panel
  printf '+ lineage: Agents panel focused — drive multi-agent tree\n'
  # Move selection through tree (brain rows / worker children)
  press_key j
  sleep_ms 0.3
  press_key j
  sleep_ms 0.3
  press_key Enter
  sleep_ms 1.0
  # Detail chrome (lowercase labels in UI)
  wait_text "stream"
  expect_text "artifacts"
  expect_text "attempts"
  printf '+ lineage: focused node detail (stream/artifacts/attempts)\n'
  # Cycle detail tabs with l (view-owned when node focused)
  press_key l
  sleep_ms 0.55
  press_key l
  sleep_ms 0.55
  soft_has_text "attempts" 2000 || true
  soft_has_text "task" 1500 || true
  soft_has_text "review" 1500 || true
  printf '+ lineage: cycled detail tabs (worker/brain outputs)\n'
  # Unfocus, move to another node (brain ↔ worker hop)
  press_key Escape
  sleep_ms 0.45
  press_key k
  sleep_ms 0.3
  press_key Enter
  sleep_ms 0.9
  soft_has_text "stream" 3000 || true
  printf '+ lineage: navigated to another agent node\n'
  press_key Escape
  sleep_ms 0.35
  # Activity log: live events from brain/worker loop
  focus_log_panel
  soft_has_text "Activity" 3000 || true
  soft_has_text "brain" 2000 || true
  printf '+ activity: event stream for auto loop visibility\n'
}

# Problem: can't see multi-agent work or how to drive the dashboard.
# Features: lineage + activity + help + palette navigation.
story_ops_visibility() {
  wait_text "Lineage"
  expect_text "Activity"
  expect_text "INSERT"
  printf '+ problem: multi-agent opacity → lineage/activity visible\n'

  # Help: how do I drive this?
  type_text "?"
  sleep_ms 0.6
  wait_text "Dashboard"
  # Soft: modes/navigation sections from help overlay
  set +e
  run_su expect text "Modes" --no-strict --timeout 4000
  run_su expect text "Navigation" --no-strict --timeout 2000
  set -e
  printf '+ problem: unknown keybindings → help overlay\n'
  press_key Escape
  sleep_ms 0.4

  # Palette: one hub for all problem surfaces
  press_key Ctrl+K
  wait_text "Go to"
  expect_text "esc dismiss"
  printf '+ problem: where is X? → command palette\n'
  press_key Escape
  sleep_ms 0.3
  wait_text "Lineage"

  # Lineage drive (Navigate mode)
  navigate_lineage_brain_and_workers
}

# Problem: multi-task campaign progress is opaque.
# Features: plan browser list + summary (progress / awaiting review).
story_plan_progress() {
  open_palette_view "Plan"
  wait_text "Plan"
  # Real projects show plan table; empty filter still has chrome
  expect_text "Progress"
  # Prefer proof of real work: awaiting review or complete rows
  set +e
  run_su expect text "awaiting" --no-strict --timeout 3000
  local has_await=$?
  run_su expect text "complete" --no-strict --timeout 2000
  local has_complete=$?
  set -e
  if [[ "$has_await" -ne 0 && "$has_complete" -ne 0 ]]; then
    expect_text "Work item"
  fi
  # Summary pane for selected plan
  set +e
  run_su expect text "Tasks" --no-strict --timeout 3000
  run_su expect text "bd-" --no-strict --timeout 2000
  set -e
  printf '+ problem: campaign opacity → plan browser progress/summary\n'
  # Cycle filter once (f cycles: all → mine → …) then restore if empty
  type_text "f"
  sleep_ms 0.5
  if soft_has_text "No plans match" 1500; then
    type_text "f"
    sleep_ms 0.4
    type_text "f"
    sleep_ms 0.4
  fi
  press_key Escape
  sleep_ms 0.4
}

# Full control-plane loop story helpers:
# Plan surface (submit_plan results) → optional start/resume → lineage drive.
#
# submit_plan itself is issued by the *brain* (MCP tool), not the TUI.
# The TUI operator path is: inspect plan browser → watch auto-dispatch on
# lineage (BRAIN / EXEC) → inspect worker/brain outputs → activity stream.
story_plan_loop_control_plane() {
  # --- Plan campaign surface (where submit_plan lands) ---
  open_palette_view "Plan"
  wait_text "Plan"
  expect_text "Progress"
  # Soft proofs — use soft_has_text (run_su re-enables set -e on failure)
  soft_has_text "Start/Resume" 3000 || true
  soft_has_text "Claim" 1500 || true
  soft_has_text "awaiting" 2000 || true
  soft_has_text "complete" 1500 || true
  printf '+ plan browser: submit_plan campaigns (progress/state)\n'
  # Move selection to surface different plan rows (campaign history)
  press_key j
  sleep_ms 0.35
  press_key j
  sleep_ms 0.35
  # Summary pane: Work item always; Progress counters live in table ("N/M done")
  expect_text "Work item"
  soft_has_text "done" 1500 || true
  soft_has_text "bd-" 1500 || true
  printf '+ plan summary: work item + progress counters\n'
  # Optional: Start/Resume selected plan (mutates live work — opt-in)
  if [[ "${SPUR_DEMO_ALLOW_PLAN_START:-0}" == "1" ]]; then
    printf '+ opt-in: Start/Resume plan (s)\n'
    type_text "s"
    sleep_ms 2.0
  fi
  press_key Escape
  sleep_ms 0.5
  wait_text "Lineage"

  # --- Lineage: brain ↔ worker navigation + output capture ---
  navigate_lineage_brain_and_workers

  # Soft proof of loop roles if live/history has them
  soft_has_text "BRAIN" 2000 || true
  soft_has_text "EXEC" 1500 || true
  soft_has_text "Running" 1000 || true
  soft_has_text "Succeeded" 1000 || true
  soft_has_text "Cancelled" 1000 || true
  printf '+ loop roles: BRAIN/EXEC states visible when present\n'
}

require_plan_loop_opt_in() {
  if [[ "${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}" != "1" && "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" != "1" ]]; then
    cat >&2 <<'EOF'
error: live plan-loop seed is opt-in (real model + possible worker spend).

  SPUR_DEMO_ALLOW_PLAN_LOOP=1 bash journeys/problem-plan-loop-drive.sh

Also accepted: SPUR_DEMO_ALLOW_AGENT_SEND=1
Optional: SPUR_DEMO_PLAN_LOOP_WAIT_S=180  (lineage EXEC wait seconds)
EOF
    return 2
  fi
}

# Poll lineage for auto-loop signals (BRAIN Running / EXEC child).
# Soft-success: never hard-fail the demo if workers are slow or brain declines.
wait_for_lineage_loop_activity() {
  local timeout_s="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-180}"
  local elapsed=0
  local step=5

  return_to_dashboard
  sleep_ms 0.4
  wait_text "Lineage"
  enter_navigate_mode
  printf '+ waiting up to %ss for lineage EXEC/Running (auto-loop)\n' "$timeout_s"

  while [[ "$elapsed" -lt "$timeout_s" ]]; do
    if soft_has_text "EXEC" 2500 || soft_has_text "Running" 2000; then
      printf '+ lineage loop activity detected at t=%ss\n' "$elapsed"
      return 0
    fi
    # Light activity refresh: focus log briefly
    if soft_has_text "Activity" 800; then
      soft_has_text "brain" 800 || true
    fi
    sleep "$step"
    elapsed=$((elapsed + step))
    printf '+ … still waiting (%ss/%ss)\n' "$elapsed" "$timeout_s"
  done
  printf '+ warn: no EXEC/Running within %ss — continue with history walk\n' "$timeout_s"
  return 0
}

# Opt-in: kick brain so auto-loop may dispatch workers (real model spend).
# Light kick — not a full submit_plan seed.
trigger_brain_for_loop_observation() {
  require_agent_send_opt_in
  attach_session_for_send
  sleep_ms 0.6
  type_text "Demo capture: reply briefly after checking lineage — say ready"
  sleep_ms 0.4
  press_key Enter
  set +e
  run_su wait text "YOU" --timeout "${SHELL_USE_TIMEOUT_MS:-90000}"
  run_su wait text "THINK" --timeout 60000
  set -e
  printf '+ triggered brain turn for loop observation\n'
  return_to_dashboard
  sleep_ms 0.5
  wait_text "Lineage"
}

# Opt-in: seed a ONE-task submit_plan via brain (real model + possible worker).
# Then wait for lineage EXEC/Running and walk brain↔worker outputs.
#
# Prompt is intentionally tiny and no-repo-write when possible so demos stay safe.
trigger_submit_plan_one_task_and_observe() {
  require_plan_loop_opt_in
  local wait_ms="${SHELL_USE_TIMEOUT_MS:-180000}"

  attach_session_for_send
  sleep_ms 0.8
  printf '+ seed: ask brain for single-task submit_plan (demo capture)\n'

  # Keep ASCII-only; avoid paste-burst by slow-typing the critical prefix.
  type_slow "DEMO CAPTURE ONLY. "
  type_text "Call submit_plan with exactly ONE task. "
  type_text "Task id: demo-echo. Worker: codex. "
  type_text "Prompt: reply with only the word ok and make no file changes. "
  type_text "deps: none. After submit_plan succeeds, reply with plan_id only."
  sleep_ms 0.5
  press_key Enter

  # Brain turn begins
  set +e
  run_su wait text "YOU" --timeout "$wait_ms"
  local you_rc=$?
  run_su wait text "THINK" --timeout 90000
  set -e
  if [[ "$you_rc" -ne 0 ]]; then
    printf '+ warn: YOU turn not observed — still polling lineage\n'
  else
    printf '+ brain turn visible (YOU) — waiting for auto-loop dispatch\n'
  fi

  # Soft: plan_id / submit language in transcript before leaving session
  soft_has_text "plan" 8000 || true
  soft_has_text "submit" 3000 || true

  # Back to dashboard — watch lineage for worker EXEC
  return_to_dashboard
  sleep_ms 0.6
  wait_for_lineage_loop_activity

  # Drive tree + capture outputs (brain vs worker if present)
  navigate_lineage_brain_and_workers

  # Re-open plan browser: new campaign should appear near top when submit landed
  open_palette_view "Plan"
  wait_text "Plan"
  expect_text "Progress"
  soft_has_text "demo" 3000 || true
  soft_has_text "awaiting" 2000 || true
  soft_has_text "running" 2000 || true
  soft_has_text "complete" 2000 || true
  printf '+ plan browser re-check after seed\n'
  press_key Escape
  sleep_ms 0.4
  wait_text "Lineage"
}

# Problem: backlog firehose — what is P0 open work?
# Features: issue browser list + detail.
story_backlog_triage() {
  open_palette_view "Issues"
  # Live monorepo uses beads issues list
  wait_text "Issues"
  expect_text "P0"
  expect_text "open"
  expect_text "bd-"
  printf '+ problem: backlog firehose → P0 open list\n'
  press_key Enter
  sleep_ms 1.0
  # Detail pane
  expect_text "bd-"
  set +e
  run_su expect text "status:" --no-strict --timeout 4000
  run_su expect text "priority:" --no-strict --timeout 2000
  set -e
  printf '+ problem: what is this issue? → detail (status/priority)\n'
  press_key Escape
  sleep_ms 0.4
}

# Machine-speed `type` is coalesced into a paste on live TUI (paste-burst
# detector), which does NOT open the @-mention cascade. Type per-character.
type_slow() {
  local s="$1"
  local delay="${2:-0.13}"
  local i
  local n=${#s}
  for ((i = 0; i < n; i++)); do
    # bash 3.2: ${s:i:1}
    run_su type -- "${s:$i:1}"
    sleep "$delay"
  done
}

# Switch between two free sessions (product: multi-session workflow).
switch_between_sessions() {
  open_sessions_picker
  expect_text "TODAY"
  sleep_ms 0.4
  press_key Down
  sleep_ms 0.3
  press_key Down
  sleep_ms 0.3
  press_key Enter
  if ! _live_complete_session_attach 8000; then
    resume_session_skip_held
  fi
  printf '+ switched to session A\n'

  # Second hop: picker → next free row
  open_sessions_picker
  sleep_ms 0.4
  press_key Down
  sleep_ms 0.25
  press_key Down
  sleep_ms 0.25
  press_key Down
  sleep_ms 0.25
  press_key Enter
  if ! _live_complete_session_attach 8000; then
    resume_session_skip_held
  fi
  printf '+ switched to session B\n'
}

# Explore: filter → star skill → Agents tab → star agent → gate accept → apply.
# Keys from ExploreBrowserView::handle_browse_key / GateState::handle_key.
explore_adopt_skill_and_agent() {
  local filter="${SPUR_DEMO_EXPLORE_FILTER:-accessibility}"
  open_explore_browser
  # Skills tab is default
  expect_text "Skills"
  # Filter
  type_text "/"
  sleep_ms 0.25
  type_text "$filter"
  sleep_ms 0.5
  press_key Enter
  sleep_ms 0.4
  # Space toggles ★ selection into pool candidate set
  press_key Space
  sleep_ms 0.35
  # Agents tab
  press_key Tab
  sleep_ms 0.45
  expect_text "Agents"
  press_key Space
  sleep_ms 0.35
  # Enter opens gate for starred items
  press_key Enter
  sleep_ms 1.0
  wait_text "Gate"
  expect_text "cards"
  # c = resolve all clean cards to Accept
  type_text "c"
  sleep_ms 0.5
  expect_text "Accept"
  # Enter applies resolved cards into pool
  press_key Enter
  sleep_ms 1.5
  wait_text "applied"
  expect_text "pool"
  printf '+ explore applied skill+agent to pool\n'
  # Optional Manage lens
  type_text "m"
  sleep_ms 0.6
  set +e
  run_su expect text "Pool" --no-strict --timeout 3000
  set -e
  # Back to browse then dashboard
  press_key Escape
  sleep_ms 0.4
  press_key Escape
  sleep_ms 0.5
  # Esc from explore → dashboard lineage
  set +e
  run_su wait text "Lineage" --timeout 5000
  set -e
}

# Cascading worker → agent profile → model → effort mention atom.
# Live projects list real personas (incl. explore-adopted agents).
compose_live_worker_cascade() {
  local worker="${SPUR_DEMO_WORKER:-codex}"
  # Prefer a clean composer surface
  attach_session_for_send
  sleep_ms 0.6
  type_slow "@worker:${worker}"
  sleep_ms 1.2
  wait_text "Mentions"
  # Slot 1: agent persona (e.g. accessibility-expert after explore adopt)
  press_key Tab
  sleep_ms 1.0
  # Slot 2: model
  press_key Tab
  sleep_ms 1.0
  # Slot 3: effort → commits final atom
  press_key Tab
  sleep_ms 1.0
  wait_text "agent="
  expect_text "model="
  expect_text "effort="
  expect_text "@worker:${worker}"
  printf '+ composed worker cascade atom\n'
}

quit_live() {
  # Live projects with attached brains use a stronger quit dialog than empty fixtures.
  press_key Ctrl+C
  # Either classic "Quit spur?" or live "agent subprocess will be terminated"
  set +e
  run_su wait text "Quit spur?" --timeout 3000
  local rc=$?
  set -e
  if [[ "$rc" -ne 0 ]]; then
    wait_text "terminated"
  fi
  press_key y
  run_su wait command --timeout "$timeout_ms"
}
