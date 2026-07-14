#!/usr/bin/env bash
# shell-use helpers for *live* project demos (no fixture isolation / no rm).
set -euo pipefail

demo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
e2e_root="$(cd "$demo_dir/../.." && pwd)"
shell_use_dir="$e2e_root/shell-use"
# shellcheck disable=SC1091
source "$e2e_root/lib/spur-bin.sh"
# Capture geometry: Mac Air M2 / wide iTerm defaults (overridable via env).
# shellcheck disable=SC1091
source "$demo_dir/../geometry.env"

cols="${SPUR_DEMO_COLS}"
rows="${SPUR_DEMO_ROWS}"
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

# Marketing film dwell — OFF by default so shell-use UAT stays fast.
# Enable for capture: SPUR_DEMO_STORY_PACE=1 (render.sh / capture-live-seed set this).
# Optional scale: SPUR_DEMO_DWELL_SCALE=1.2
story_pace_on() {
  [[ "${SPUR_DEMO_STORY_PACE:-0}" == "1" ]]
}

story_dwell() {
  # Args: seconds (float, default 2.5). No-op unless SPUR_DEMO_STORY_PACE=1.
  if ! story_pace_on; then
    return 0
  fi
  local base="${1:-2.5}"
  local scale="${SPUR_DEMO_DWELL_SCALE:-1}"
  local secs
  secs="$(awk -v b="$base" -v s="$scale" 'BEGIN { printf "%.2f", (b + 0) * (s + 0) }')"
  printf '+ story_dwell %ss\n' "$secs"
  sleep "$secs"
}

# Hop delay: short for UAT, readable for film.
story_hop() {
  if story_pace_on; then
    sleep_ms "${1:-1.2}"
  else
    sleep_ms "${2:-0.35}"
  fi
}

# Narrative markers appear in shell-use logs and keep every value journey on the
# same hook → orientation → action → proof → resolution spine.
story_beat() {
  local stage="$1"
  local message="$2"
  printf '\n== story · %s · %s ==\n' "$stage" "$message"
}

# A hard proof is part of the UAT contract: the journey stops if the anchor is
# not visible. Film-only dwell begins only after the screen is stable.
story_hard_proof() {
  local claim="$1"
  local anchor="$2"
  local dwell="${3:-3.0}"
  wait_text "$anchor"
  printf '+ proof: %s [anchor=%s]\n' "$claim" "$anchor"
  story_dwell "$dwell"
}

# Live projects legitimately differ. Optional history/state must never masquerade
# as proof: label both the observed and absent paths, then continue safely.
story_soft_proof() {
  local claim="$1"
  local anchor="$2"
  local timeout="${3:-2000}"
  local dwell="${4:-2.5}"
  local absent="${5:-optional anchor is absent on this project}"

  if soft_has_text "$anchor" "$timeout"; then
    printf '+ proof: %s [anchor=%s]\n' "$claim" "$anchor"
    story_dwell "$dwell"
  else
    printf '+ soft beat: %s — %s [missing=%s]\n' "$claim" "$absent" "$anchor"
  fi
}

# Dashboard has two legitimate render paths: Lineage/Activity when history is
# present, and a compose-ready splash when the lineage projection is empty.
# Poll both so empty projects are labeled, fast, and never treated as proof.
story_dashboard_land() {
  local claim="${1:-The live dashboard is ready}"
  local dwell="${2:-2.5}"
  local attempt

  for attempt in {1..20}; do
    if soft_has_text "Lineage" 250; then
      printf '+ proof: %s [anchor=Lineage]\n' "$claim"
      story_dwell "$dwell"
      return 0
    fi
    if soft_has_text "Type a task below" 250; then
      printf '+ soft beat: %s — no active lineage yet; compose-ready dashboard is visible\n' "$claim"
      story_dwell "$dwell"
      return 0
    fi
    if soft_has_text "No agents configured" 250; then
      printf '+ soft beat: %s — dashboard is visible, but this project still needs agent setup\n' "$claim"
      story_dwell "$dwell"
      return 0
    fi
  done

  # Preserve a hard startup invariant without pretending a specific data state.
  wait_text "SPUR"
  printf '+ soft beat: %s — dashboard loaded without a known lineage/splash anchor\n' "$claim"
  story_dwell "$dwell"
}

story_resolution() {
  local message="$1"
  local dwell="${2:-2.5}"
  story_beat "RESOLUTION" "$message"
  story_dwell "$dwell"
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
    if dashboard_is_visible; then
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

dashboard_is_visible() {
  soft_has_text "Lineage" 300 \
    || soft_has_text "Type a task below" 300 \
    || soft_has_text "No agents configured" 300
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
  run_su wait text "Start new session|No sessions yet" --regex --timeout "$timeout_ms"
}

# Returns 0 if screen shows attached session detail.
_live_wait_session_detail() {
  local ms="${1:-6000}"
  set +e
  "$shell_use_bin" --session "$session_name" wait text "Session ·" --timeout "$ms" >/dev/null 2>&1
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
    press_key y
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

# Return the first visible non-empty composer line, or no output for an empty
# composer. shell-use emits plain terminal text, so this deliberately scopes
# extraction to the line after the INSERT header and before the lower border.
_live_composer_first_text_line() {
  "$shell_use_bin" --session "$session_name" text | awk '
    index($0, "● INSERT") > 0 { in_composer = 1; next }
    in_composer && /^[[:space:]]*─/ { exit }
    in_composer {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
      if (length(line) > 0) {
        print line
        exit
      }
    }
  '
}

# Find a clean local session for a no-send composer proof. Reusing an arbitrary
# session can restore its unsent draft; appending @worker to that draft makes
# repeat runs misleading and risks overwriting operator work.
start_clean_session_for_draft() {
  local worker="${1:-codex}"
  local candidate_row current_draft step

  # Cursor 0 is [+ Start new session]; rows 1..N are persisted sessions.
  # Inspect candidates without clearing any draft. A controlled empty project
  # uses its sole clean session; a busy project can fall through to another.
  for candidate_row in 1 2 3 4; do
    open_sessions_picker
    if soft_has_text "No sessions yet" 1000; then
      press_key Enter
    else
      for ((step = 0; step < candidate_row; step++)); do
        press_key Down
        sleep_ms 0.15
      done
      press_key Enter
    fi

    if ! _live_complete_session_attach 8000; then
      printf '+ soft beat: clean composer search — candidate %s could not attach\n' "$candidate_row"
      continue
    fi
    current_draft="$(_live_composer_first_text_line || true)"
    if [[ -z "$current_draft" ]]; then
      printf '+ proof: clean local session protects existing drafts while the dispatch atom is composed\n'
      story_dwell 2.0
      return 0
    fi
    if [[ "$current_draft" == "@worker:${worker}"* \
      && "$current_draft" == *"agent="* \
      && "$current_draft" == *"model="* \
      && "$current_draft" == *"effort="* ]]; then
      printf '+ proof: configured worker draft already exists; preserve and reuse it\n'
      story_dwell 2.0
      return 2
    fi
    printf '+ soft beat: clean composer search — candidate %s has a draft; preserve it and continue\n' "$candidate_row"
  done

  printf 'error: every attachable composer has text; refusing to overwrite an operator draft\n' >&2
  return 1
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
  story_hop 1.0 0.4
  if ! soft_has_text "[Agents]" 1200; then
    press_key Tab
    story_hop 1.0 0.4
  fi
  story_soft_proof \
    "Agents tree owns keyboard focus" \
    "[Agents]" 2000 2.0 \
    "focus marker is hidden, so the journey continues from the visible Lineage tree"
}

# Focus Activity log panel.
focus_log_panel() {
  enter_navigate_mode
  press_key Tab
  story_hop 0.9 0.35
  if ! soft_has_text "[Log]" 1000; then
    press_key Tab
    story_hop 0.9 0.35
  fi
}

# Walk lineage: select nodes, open detail (stream/artifacts/attempts/task/review).
# Proves brain↔worker tree is navigable and outputs are inspectable.
# Try to open lineage node detail; soft if tree/layout differs mid-loop.
_lineage_focus_selected_node() {
  press_key Enter
  story_hop 1.5 1.0
  if soft_has_text "stream" 6000; then
    return 0
  fi
  # Retry: leave any overlay, re-focus Agents, select first, Enter
  press_key Escape
  story_hop 0.8 0.35
  focus_agents_panel
  # 'g' = first node (view-action in Navigate)
  type_text "g"
  story_hop 0.8 0.35
  press_key Enter
  story_hop 1.5 1.0
  if soft_has_text "stream" 8000; then
    return 0
  fi
  return 1
}

# Explicit story beats: Agents → prefer BRAIN row → EXEC/worker → stream → tabs.
# Soft when roles absent (empty project); still navigates tree for film continuity.
navigate_lineage_brain_and_workers() {
  if ! soft_has_text "Lineage" 800; then
    printf '+ soft beat: BRAIN→EXEC lineage — no active or historical lineage exists on this project\n'
    printf '+ soft beat: worker output/review — seed a plan loop to make these panes provable\n'
    printf '+ soft beat: Activity timeline — no agent events exist yet\n'
    story_dwell 2.5
    return 0
  fi

  focus_agents_panel
  printf '+ lineage: Agents panel focused — drive multi-agent tree\n'

  # Beat: surface BRAIN role when present (control plane owner)
  story_soft_proof \
    "BRAIN row identifies the control-plane owner" \
    "BRAIN" 2500 2.5 \
    "no brain history is present; the Agents tree remains the orientation proof"

  # Move selection through tree (brain rows / worker children)
  press_key j
  story_hop 1.2 0.3
  press_key j
  story_hop 1.2 0.3

  # Beat: EXEC / Running when live loop has workers
  if soft_has_text "EXEC" 2000 || soft_has_text "Running" 1500; then
    printf '+ proof: EXEC/Running row identifies delegated work [anchor=EXEC|Running]\n'
    story_dwell 2.5
  else
    printf '+ soft beat: delegated worker row — no EXEC/Running history on this project\n'
  fi

  if _lineage_focus_selected_node; then
    printf '+ proof: selected agent exposes live output [anchor=stream]\n'
    story_dwell 3.5
    # Cycle detail tabs with l (view-owned when node focused)
    press_key l
    story_hop 1.8 0.55
    story_soft_proof \
      "Attempts/artifacts make worker history inspectable" \
      "attempts" 2000 1.8 \
      "this node has no attempt history; stream proof remains visible"
    press_key l
    story_hop 1.8 0.55
    story_soft_proof \
      "Task/review context closes the worker-to-brain loop" \
      "task" 1500 1.8 \
      "this node has no task payload; the tab walk is still labeled"
    printf '+ lineage: cycled detail tabs (stream → attempts/artifacts → task/review)\n'
  else
    printf '+ soft beat: agent output detail — no selectable lineage node on this project\n'
  fi
  # Unfocus, hop to another node (brain ↔ worker)
  press_key Escape
  story_hop 1.0 0.45
  press_key k
  story_hop 1.2 0.3
  if _lineage_focus_selected_node; then
    printf '+ proof: brain↔worker selection changes without losing the control-plane view\n'
    story_dwell 2.0
  else
    printf '+ soft beat: brain↔worker hop — only one selectable lineage node is present\n'
  fi
  press_key Escape
  story_hop 0.8 0.35
  # Activity log: live events from brain/worker loop
  focus_log_panel
  story_hard_proof "Activity is the operator timeline for the loop" "Activity" 3.0
}

# Problem: can't see multi-agent work or how to drive the dashboard.
# Features: lineage + activity + help + palette navigation.
story_ops_visibility() {
  story_soft_proof \
    "Activity answers what has happened" \
    "Activity" 1200 2.5 \
    "no agent events exist yet; the empty dashboard is labeled instead"
  expect_text "INSERT"

  # Help: how do I drive this?
  type_text "?"
  story_hop 1.0 0.6
  story_hard_proof "Dashboard help turns unknown keys into an operating guide" "Dashboard" 3.0
  story_soft_proof \
    "Navigation guidance explains how to drive the view" \
    "Navigation" 2000 2.0 \
    "compact help layout omits the section heading"
  press_key Escape
  story_hop 0.8 0.4

  # Palette: one hub for all problem surfaces
  press_key Ctrl+K
  story_hard_proof "Go to is the hub for every control surface" "Go to" 2.5
  expect_text "esc dismiss"
  press_key Escape
  story_hop 0.8 0.3
  story_dashboard_land "Return from Go to without losing dashboard context" 2.0

  # Lineage drive (Navigate mode)
  navigate_lineage_brain_and_workers
}

# Problem: multi-task campaign progress is opaque.
# Features: plan browser list + summary (progress / awaiting review).
story_plan_progress() {
  local has_plan_rows=0
  open_palette_view "Plan"
  story_hard_proof "Plans is the campaign control surface" "Plans" 2.5
  if soft_has_text "Progress" 1500; then
    has_plan_rows=1
    printf '+ proof: campaign rows turn multi-task work into visible progress [anchor=Progress]\n'
    story_dwell 3.5
  elif soft_has_text "No plans found" 1500; then
    printf '+ soft beat: campaign progress — no plans found; seed a campaign to populate this surface\n'
    story_dwell 2.5
  else
    printf '+ soft beat: campaign progress — Plans opened without rows or the known empty-state copy\n'
  fi

  if [[ "$has_plan_rows" -eq 1 ]]; then
    # Prefer proof of real work; label history without broad empty-screen matches.
    if soft_has_text "awaiting" 3000 || soft_has_text "running" 1500 || soft_has_text "complete" 2000; then
      printf '+ proof: campaign rows expose running/awaiting/complete state\n'
      story_dwell 3.0
    else
      printf '+ soft beat: campaign state — selected plan has no visible lifecycle word\n'
    fi

    story_soft_proof \
      "Work Item summary ties progress to the campaign objective" \
      "Work Item" 3000 3.5 \
      "the selected campaign has no visible objective summary"
    story_soft_proof \
      "Task rows show the campaign breakdown" \
      "Tasks" 2000 2.5 \
      "the selected campaign has no task summary on screen"

    # Cycle filter once (f cycles: all → mine → …) then restore if empty.
    type_text "f"
    story_hop 1.0 0.5
    if soft_has_text "No plans match" 1500; then
      printf '+ soft beat: filtered campaign list — no plans match; restore the all-plans view\n'
      story_dwell 2.0
      # We are on Mine. Cycle Mine → Unowned → Active → Terminal → All.
      for _ in 1 2 3 4; do
        type_text "f"
        story_hop 0.8 0.4
      done
    fi
  else
    printf '+ soft beat: campaign lifecycle/objective/tasks — no plan rows exist to inspect\n'
  fi
}

# Full control-plane loop story helpers:
# Plan surface (submit_plan results) → optional start/resume → lineage drive.
#
# submit_plan itself is issued by the *brain* (MCP tool), not the TUI.
# The TUI operator path is: inspect plan browser → watch auto-dispatch on
# lineage (BRAIN / EXEC) → inspect worker/brain outputs → activity stream.
story_plan_loop_control_plane() {
  local has_plan_rows=0
  # --- Plan campaign surface (where submit_plan lands) ---
  open_palette_view "Plan"
  story_hard_proof "submit_plan campaigns land in the Plans surface" "Plans" 2.5
  if soft_has_text "Progress" 1500; then
    has_plan_rows=1
    printf '+ proof: plan table exposes campaign progress [anchor=Progress]\n'
    story_dwell 3.5
  elif soft_has_text "No plans found" 1500; then
    printf '+ soft beat: submit_plan campaign history — no plans found on this project\n'
    story_dwell 2.5
  else
    printf '+ soft beat: submit_plan campaign history — Plans opened without rows or known empty copy\n'
  fi
  story_soft_proof \
    "Start/Resume makes campaign control explicit" \
    "Start/Resume" 3000 2.5 \
    "no resumable campaign exists; the control remains safely unavailable"

  if [[ "$has_plan_rows" -eq 1 ]]; then
    if soft_has_text "awaiting" 2000 || soft_has_text "running" 1500 || soft_has_text "complete" 1500; then
      printf '+ proof: campaign lifecycle state is visible in the plan table\n'
      story_dwell 3.0
    else
      printf '+ soft beat: campaign lifecycle — selected plan has no visible lifecycle word\n'
    fi

    # Move selection to surface different plan rows (campaign history).
    press_key j
    story_hop 1.2 0.35
    press_key j
    story_hop 1.2 0.35
    story_soft_proof \
      "Work Item and counters connect campaign state to operator intent" \
      "Work Item" 2500 3.0 \
      "the selected campaign has no visible summary"
    story_soft_proof \
      "Done counters quantify progress across tasks" \
      "done" 1500 2.0 \
      "this project has no completed plan counters yet"

    # Optional: Start/Resume selected plan (mutates live work — opt-in).
    if [[ "${SPUR_DEMO_ALLOW_PLAN_START:-0}" == "1" ]]; then
      printf '+ opt-in: Start/Resume plan (s)\n'
      type_text "s"
      story_dwell 3.0
    fi
  else
    printf '+ soft beat: campaign lifecycle/summary — no plan rows exist to inspect\n'
    if [[ "${SPUR_DEMO_ALLOW_PLAN_START:-0}" == "1" ]]; then
      printf '+ soft beat: Start/Resume skipped — no selected plan\n'
    fi
  fi
  press_key Escape
  story_hop 1.0 0.5
  story_dashboard_land "Return from Plans to the loop dashboard" 2.0

  # --- Lineage: brain ↔ worker navigation + output capture ---
  # Explicit arc: BRAIN → EXEC → stream → tabs → Activity (inside helper)
  navigate_lineage_brain_and_workers
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
  local seed_task_id="${1:-}"
  local timeout_s="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-180}"
  local elapsed=0
  local step=5
  local generic_seen=0

  return_to_dashboard
  sleep_ms 0.4
  story_dashboard_land "Dashboard is ready for post-seed observation" 2.0
  printf '+ waiting up to %ss for lineage correlated to task %s\n' "$timeout_s" "$seed_task_id"

  while [[ "$elapsed" -lt "$timeout_s" ]]; do
    if [[ -n "$seed_task_id" ]] && soft_has_text "$seed_task_id" 2500; then
      printf '+ proof: lineage exposes the per-run seed task %s at t=%ss\n' "$seed_task_id" "$elapsed"
      story_dwell 3.5
      return 0
    fi
    if [[ "$generic_seen" -eq 0 ]] \
      && { soft_has_text "EXEC" 1200 || soft_has_text "Running" 1000; }; then
      printf '+ soft beat: EXEC/Running is visible but may be pre-existing; keep waiting for %s\n' "$seed_task_id"
      generic_seen=1
    fi
    sleep "$step"
    elapsed=$((elapsed + step))
    printf '+ … still waiting (%ss/%ss)\n' "$elapsed" "$timeout_s"
  done
  printf '+ soft beat: seed correlation — task %s was not visible in lineage within %ss; generic history is not attributed to this run\n' "$seed_task_id" "$timeout_s"
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
  story_dashboard_land "Return to the dashboard after the brain kick" 2.0
}

# Opt-in: seed a ONE-task submit_plan via brain (real model + possible worker).
# Then wait for lineage EXEC/Running and walk brain↔worker outputs.
#
# Prompt is intentionally tiny and no-repo-write when possible so demos stay safe.
trigger_submit_plan_one_task_and_observe() {
  require_plan_loop_opt_in
  local wait_ms="${SHELL_USE_TIMEOUT_MS:-180000}"
  local seed_task_id="demo-echo-$$"
  local plan_correlated=0

  attach_session_for_send
  sleep_ms 0.8
  printf '+ seed: ask brain for single-task submit_plan (demo capture)\n'

  # Keep ASCII-only; avoid paste-burst by slow-typing the critical prefix.
  type_slow "DEMO CAPTURE ONLY. "
  type_text "Call submit_plan with exactly ONE task. "
  type_text "Task id: ${seed_task_id}. Worker: codex. "
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
    printf '+ soft beat: brain accepted seed — YOU was not observed; still poll lineage\n'
  else
    printf '+ proof: brain accepted the seed turn [anchor=YOU]\n'
    story_dwell 2.5
  fi

  # This anchor is deliberately action proof only: the visible user prompt
  # contains plan language, so it cannot prove that the brain submitted one.
  story_soft_proof \
    "Seed prompt records the requested submit_plan action" \
    "plan" 8000 2.5 \
    "the submitted seed prompt was not visible before the lineage poll"

  # Back to dashboard — watch lineage for worker EXEC
  return_to_dashboard
  sleep_ms 0.6
  wait_for_lineage_loop_activity "$seed_task_id"

  # Drive tree + capture outputs (brain vs worker if present)
  navigate_lineage_brain_and_workers
  story_dwell 2.5

  # Re-open Plan browser and attribute results only when the per-run task tag
  # is visible. Generic progress/history may predate this seed.
  open_palette_view "Plan"
  story_hard_proof "Plans is ready for post-seed inspection" "Plans" 2.5
  if soft_has_text "$seed_task_id" 5000; then
    plan_correlated=1
    printf '+ proof: Plans exposes the per-run task tag %s\n' "$seed_task_id"
    story_dwell 3.5
  else
    printf '+ soft beat: seeded plan correlation — %s is not visible; existing rows remain historical evidence\n' "$seed_task_id"
  fi
  if soft_has_text "Progress" 3000; then
    if [[ "$plan_correlated" -eq 1 ]]; then
      printf '+ proof: the correlated seeded campaign exposes visible progress\n'
      story_dwell 3.5
    else
      printf '+ soft beat: campaign progress is visible but cannot be attributed to this seed\n'
    fi
  else
    printf '+ soft beat: post-seed campaign progress — no Progress anchor is visible\n'
  fi
  if soft_has_text "awaiting" 2000 || soft_has_text "running" 2000 || soft_has_text "complete" 2000; then
    if [[ "$plan_correlated" -eq 1 ]]; then
      printf '+ proof: the correlated seeded plan exposes a current lifecycle state\n'
      story_dwell 3.0
    else
      printf '+ soft beat: lifecycle state is visible but may belong to existing history\n'
    fi
  else
    printf '+ soft beat: post-seed lifecycle state — state text has not materialized yet\n'
  fi
  press_key Escape
  story_hop 0.8 0.4
  story_dashboard_land "Return from Plans to the loop dashboard" 2.0
}

# Problem: backlog firehose — what is P0 open work?
# Features: issue browser list + detail.
story_backlog_triage() {
  open_palette_view "Issues"
  # Live monorepo uses beads issues list
  story_hard_proof "Issues is the dedicated backlog decision surface" "Issues" 2.5

  if soft_has_text "Failed to load issues" 1200 || soft_has_text "load failed" 500; then
    printf 'error: backlog unavailable — issue loading failed, so an empty queue cannot be claimed\n' >&2
    return 1
  fi
  if soft_has_text "No issues loaded" 1000; then
    printf '+ soft beat: P0 firehose — the loaded project has no issues\n'
    story_dwell 2.5
    return 0
  fi

  if soft_has_text "P0" 2500 && soft_has_text "open" 1500 && soft_has_text "bd-" 1500; then
    printf '+ orientation: P0, open, and issue IDs are visible; selected-row urgency is verified in detail\n'
    story_dwell 3.5
  else
    printf '+ soft beat: P0 firehose — this project has no visible open P0 issue\n'
    return 0
  fi

  press_key Enter
  story_hop 1.5 1.0
  # Bind proof to one selected detail pane. Generic list-wide P0/open/bd-
  # matches can come from different rows and must not establish urgency.
  if soft_has_text "status: open" 3000 && soft_has_text "priority: P0" 2000; then
    printf '+ proof: selected issue detail binds its identity to status open and priority P0\n'
    story_dwell 4.0
  else
    printf '+ soft beat: selected issue detail — status open and priority P0 were not proven together\n'
  fi
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

# Resume one prior session without claiming two distinct rows when the project
# only has one. The visible resume marker is project-dependent evidence.
resume_prior_session_context() {
  open_sessions_picker
  if soft_has_text "No sessions yet" 1500; then
    printf '+ soft beat: session continuity — no prior sessions exist on this project\n'
    printf '+ action: start one clean local session so specialist configuration can continue without sending\n'
    press_key Enter
    if ! _live_complete_session_attach 10000; then
      wait_text "INSERT"
    fi
    printf '+ proof: clean session is ready; no model turn was sent\n'
    story_dwell 2.5
    return 0
  fi

  story_hard_proof "Session history keeps prior context recoverable" "TODAY" 3.0
  press_key Down
  story_hop 0.9 0.3
  press_key Down
  story_hop 0.9 0.3
  press_key Enter
  if ! _live_complete_session_attach 8000; then
    resume_session_skip_held
  fi
  story_hard_proof "A saved session opens in its detail surface" "Session ·" 2.5
  story_soft_proof \
    "Resume marker confirms prior conversation context" \
    "Resumed from prior conversation" 2500 3.0 \
    "the saved session has no visible resume marker; no second distinct session is claimed"
}

# Explore: filter → star skill → Agents tab → star agent → gate accept → apply.
# Keys from ExploreBrowserView::handle_browse_key / GateState::handle_key.
explore_adopt_skill_and_agent() {
  local filter="${SPUR_DEMO_EXPLORE_FILTER:-accessibility}"
  open_explore_browser
  # Skills tab is default
  story_hard_proof "Synced catalog supplies reusable specialist capabilities" "Skills" 2.5
  # Filter
  type_text "/"
  story_hop 0.6 0.25
  type_text "$filter"
  story_hop 1.0 0.5
  press_key Enter
  story_hop 0.9 0.4
  # Space toggles ★ selection into pool candidate set
  press_key Space
  story_hop 0.9 0.35
  # Agents tab
  press_key Tab
  story_hop 1.0 0.45
  expect_text "Agents"
  press_key Space
  story_hop 0.9 0.35
  # Enter opens gate for starred items
  press_key Enter
  story_hop 1.5 1.0
  story_hard_proof "Gate makes specialist adoption an explicit trust decision" "Gate" 3.0
  expect_text "cards"
  # c = resolve all clean cards to Accept
  type_text "c"
  story_hop 1.0 0.5
  expect_text "Accept"
  # Enter applies resolved cards into pool
  press_key Enter
  story_hop 2.0 1.5
  story_hard_proof "Accepted skill and agent are applied to the local pool" "applied" 4.0
  expect_text "pool"
  # Optional Manage lens
  type_text "m"
  story_hop 1.2 0.6
  story_soft_proof \
    "Pool view confirms the adopted specialist remains available" \
    "Pool" 3000 2.0 \
    "compact Manage view does not expose the Pool heading"
  # Back to browse then dashboard
  press_key Escape
  story_hop 0.8 0.4
  press_key Escape
  story_hop 1.0 0.5
  # Esc from explore → dashboard lineage
  story_dashboard_land "Return to the dashboard with the specialist in the pool" 2.0
}

prove_worker_cascade_atom() {
  local worker="$1"
  # Strict cascade proof — all three must appear (marketing + UAT)
  wait_text "agent="
  printf '+ proof: agent profile locks the specialist persona [anchor=agent=]\n'
  wait_text "model="
  printf '+ proof: model selection is explicit [anchor=model=]\n'
  wait_text "effort="
  printf '+ proof: effort selection is explicit [anchor=effort=]\n'
  expect_text "@worker:${worker}"
  printf '+ composed worker cascade atom (agent= model= effort= proven)\n'
  story_dwell 4.0
}

# Cascading worker → agent profile → model → effort mention atom.
# Live projects list real personas (incl. explore-adopted agents).
# Proof requires all three of agent= / model= / effort= on screen (not partial).
compose_live_worker_cascade() {
  local worker="${SPUR_DEMO_WORKER:-codex}"
  local composer_rc=0
  # Use a dedicated clean composer surface so repeat captures never append to
  # an operator's restored unsent draft.
  if start_clean_session_for_draft "$worker"; then
    composer_rc=0
  else
    composer_rc=$?
  fi
  if [[ "$composer_rc" -eq 2 ]]; then
    prove_worker_cascade_atom "$worker"
    return 0
  fi
  if [[ "$composer_rc" -ne 0 ]]; then
    return "$composer_rc"
  fi
  story_hop 1.0 0.6
  type_slow "@worker:${worker}"
  story_hop 1.5 1.2
  story_hard_proof "Mentions opens dispatch configuration in context" "Mentions" 2.5
  # Slot 1: agent persona (e.g. accessibility-expert after explore adopt)
  press_key Tab
  story_hop 1.5 1.0
  # Slot 2: model
  press_key Tab
  story_hop 1.5 1.0
  # Slot 3: effort → commits final atom
  press_key Tab
  story_hop 1.5 1.0
  prove_worker_cascade_atom "$worker"
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
