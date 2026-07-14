#!/usr/bin/env bash
# Live-project dual runner: shell-use UAT + VHS demo capture.
#
# Usage:
#   ./uat.sh                              # safe journeys UAT + capture
#   ./uat.sh --mode uat
#   ./uat.sh --mode capture
#   SPUR_DEMO_ALLOW_AGENT_SEND=1 ./uat.sh # include real agent send (costs tokens)
#   SPUR_DEMO_PROJECT=/path/to/repo ./uat.sh --mode capture
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"

mode="all"
list_only=false

usage() {
  cat <<'USAGE'
Usage: uat.sh [--mode uat|capture|all] [--list]

Live project demos on a real .spur/ workspace.

Env:
  SPUR_DEMO_PROJECT              project root (default: this monorepo)
  SPUR_BIN                       spur binary
  SPUR_DEMO_ALLOW_AGENT_SEND=1   include agent-send journey (REAL model spend)
  SHELL_USE_TIMEOUT_MS           wait timeout (agent-send defaults higher internally)
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      mode="${2:-}"
      shift 2
      ;;
    --list)
      list_only=true
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$mode" in
  uat | capture | all) ;;
  *)
    echo "error: --mode must be uat|capture|all" >&2
    exit 2
    ;;
esac

read_journeys() {
  awk -F'|' '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    NF >= 2 {
      flags = (NF >= 4 ? $4 : "")
      print $1 "|" $2 "|" (NF >= 3 ? $3 : "") "|" flags
    }
  ' "$ROOT/journeys.conf"
}

if [[ "$list_only" == true ]]; then
  printf '%-22s %-28s %-24s %s\n' "JOURNEY" "SHELL-USE" "VHS_TAPE" "FLAGS"
  while IFS='|' read -r name script tape flags; do
    printf '%-22s %-28s %-24s %s\n' "$name" "$script" "${tape:-(none)}" "${flags:-(safe)}"
  done < <(read_journeys)
  exit 0
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN
export SPUR_DEMO_PROJECT="${SPUR_DEMO_PROJECT:-$(git -C "$E2E_ROOT/../.." rev-parse --show-toplevel)}"
allow_send="${SPUR_DEMO_ALLOW_AGENT_SEND:-0}"

echo "=== tui-live (real project) ==="
echo "mode:                      $mode"
echo "SPUR_BIN:                  $SPUR_BIN"
echo "SPUR_DEMO_PROJECT:         $SPUR_DEMO_PROJECT"
echo "SPUR_DEMO_ALLOW_AGENT_SEND:$allow_send"
echo

failures=0
skipped=0

run_uat() {
  echo "--- shell-use UAT (live) ---"
  while IFS='|' read -r name script _tape flags; do
    if [[ "$flags" == *agent-send* && "$allow_send" != "1" ]]; then
      echo "SKIP uat ${name} (set SPUR_DEMO_ALLOW_AGENT_SEND=1)"
      skipped=$((skipped + 1))
      continue
    fi
    path="$ROOT/journeys/$script"
    if [[ ! -f "$path" ]]; then
      echo "FAIL uat ${name} missing=${path}" >&2
      failures=$((failures + 1))
      continue
    fi
    echo
    echo "=== UAT: ${name} ==="
    if SPUR_BIN="$SPUR_BIN" \
      SPUR_DEMO_PROJECT="$SPUR_DEMO_PROJECT" \
      SPUR_DEMO_ALLOW_AGENT_SEND="$allow_send" \
      bash "$path"; then
      echo "PASS uat ${name}"
    else
      echo "FAIL uat ${name}" >&2
      failures=$((failures + 1))
    fi
  done < <(read_journeys)
}

run_capture() {
  echo "--- VHS capture (live) ---"
  if ! SPUR_DEMO_ALLOW_AGENT_SEND="$allow_send" "$ROOT/render.sh"; then
    failures=$((failures + 1))
  fi
}

case "$mode" in
  uat) run_uat ;;
  capture) run_capture ;;
  all)
    run_uat
    if [[ "$failures" -eq 0 ]]; then
      run_capture
    else
      echo "skipping capture: UAT had failures" >&2
    fi
    ;;
esac

echo
echo "=== summary ==="
echo "failures: $failures"
echo "skipped:  $skipped (agent-send gated)"
if [[ "$failures" -ne 0 ]]; then
  exit 1
fi
if [[ "$mode" == "capture" || "$mode" == "all" ]]; then
  echo "media: $ROOT/out/"
fi
