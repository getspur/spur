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

open_explore_browser() {
  blur_composer
  press_key Ctrl+K
  wait_text "Go to"
  type_text "Explore"
  sleep_ms 0.4
  press_key Enter
  wait_text "Explore"
  expect_text "synced"
  expect_text "catalog"
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
