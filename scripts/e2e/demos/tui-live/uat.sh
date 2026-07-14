#!/usr/bin/env bash
# Live-project dual runner: shell-use UAT + VHS demo capture.
#
# Usage:
#   ./uat.sh                      # UAT then capture
#   ./uat.sh --mode uat
#   ./uat.sh --mode capture
#   SPUR_DEMO_PROJECT=/path/to/repo ./uat.sh --mode capture
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"

mode="all"

usage() {
  cat <<'USAGE'
Usage: uat.sh [--mode uat|capture|all] [--list]

Live project demos — navigation only (no agent prompts).

  SPUR_DEMO_PROJECT   project root with .spur/ (default: this monorepo)
  SPUR_BIN            spur binary
USAGE
}

list_only=false
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
    NF >= 2 { print $1 "|" $2 "|" (NF >= 3 ? $3 : "") }
  ' "$ROOT/journeys.conf"
}

if [[ "$list_only" == true ]]; then
  printf '%-22s %-28s %s\n' "JOURNEY" "SHELL-USE" "VHS_TAPE"
  while IFS='|' read -r name script tape; do
    printf '%-22s %-28s %s\n' "$name" "$script" "${tape:-(none)}"
  done < <(read_journeys)
  exit 0
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN
export SPUR_DEMO_PROJECT="${SPUR_DEMO_PROJECT:-$(git -C "$E2E_ROOT/../.." rev-parse --show-toplevel)}"

echo "=== tui-live (real project) ==="
echo "mode:             $mode"
echo "SPUR_BIN:         $SPUR_BIN"
echo "SPUR_DEMO_PROJECT:$SPUR_DEMO_PROJECT"
echo

failures=0

run_uat() {
  echo "--- shell-use UAT (live) ---"
  while IFS='|' read -r name script _tape; do
    path="$ROOT/journeys/$script"
    if [[ ! -f "$path" ]]; then
      echo "FAIL uat ${name} missing=${path}" >&2
      failures=$((failures + 1))
      continue
    fi
    echo
    echo "=== UAT: ${name} ==="
    if SPUR_BIN="$SPUR_BIN" SPUR_DEMO_PROJECT="$SPUR_DEMO_PROJECT" bash "$path"; then
      echo "PASS uat ${name}"
    else
      echo "FAIL uat ${name}" >&2
      failures=$((failures + 1))
    fi
  done < <(read_journeys)
}

run_capture() {
  echo "--- VHS capture (live) ---"
  if ! "$ROOT/render.sh"; then
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
if [[ "$failures" -ne 0 ]]; then
  exit 1
fi
if [[ "$mode" == "capture" || "$mode" == "all" ]]; then
  echo "media: $ROOT/out/"
fi
