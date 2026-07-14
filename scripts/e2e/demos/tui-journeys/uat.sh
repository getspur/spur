#!/usr/bin/env bash
# Dual-purpose Arc A runner:
#   UAT     — shell-use journeys (behavioral feature acceptance)
#   capture — VHS media (mp4/gif for demos later)
#
# Story catalog: journeys.conf (must stay aligned with JOURNEYS.md).
#
# Usage:
#   ./uat.sh                    # UAT then capture
#   ./uat.sh --mode uat
#   ./uat.sh --mode capture
#   ./uat.sh --mode all
#   ./uat.sh --list
#   SPUR_BIN=... ./uat.sh --mode uat
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
SHELL_USE_DIR="$E2E_ROOT/shell-use"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"

mode="all"
list_only=false

usage() {
  cat <<'USAGE'
Usage: uat.sh [--mode uat|capture|all] [--list]

  --mode uat       Run shell-use journeys only (feature UAT)
  --mode capture   Run VHS media capture only (demo assets → out/)
  --mode all       UAT then capture (default)
  --list           Print journeys.conf rows and exit

Env:
  SPUR_BIN              path to spur binary (default: target/debug/spur)
  SHELL_USE_BIN         override shell-use binary
  SHELL_USE_TIMEOUT_MS  wait timeout for shell-use (default from lib)
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
    echo "error: --mode must be uat, capture, or all (got: $mode)" >&2
    exit 2
    ;;
esac

read_journeys() {
  # Emits: name|fixture|script|tape_stem
  awk -F'|' '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    NF >= 3 {
      name = $1
      fixture = $2
      script = $3
      tape = (NF >= 4 ? $4 : "")
      print name "|" fixture "|" script "|" tape
    }
  ' "$ROOT/journeys.conf"
}

if [[ "$list_only" == true ]]; then
  printf '%-24s %-16s %-28s %s\n' "JOURNEY" "FIXTURE" "SHELL-USE" "VHS_TAPE"
  printf '%-24s %-16s %-28s %s\n' "-------" "-------" "---------" "--------"
  while IFS='|' read -r name fixture script tape; do
    printf '%-24s %-16s %-28s %s\n' "$name" "$fixture" "$script" "${tape:-(uat only)}"
  done < <(read_journeys)
  exit 0
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN

artifact_root="${SPUR_E2E_ARTIFACTS_DIR:-$ROOT/.artifacts}"
mkdir -p "$artifact_root"

echo "=== tui-journeys Arc A ==="
echo "mode:     $mode"
echo "SPUR_BIN: $SPUR_BIN"
echo "artifacts:$artifact_root"
echo

failures=0

run_uat() {
  local shell_use_bin
  shell_use_bin="${SHELL_USE_BIN:-"$("$SHELL_USE_DIR/install.sh")"}"
  if [[ ! -x "$shell_use_bin" ]]; then
    echo "error: shell-use binary not executable: $shell_use_bin" >&2
    return 2
  fi
  export SHELL_USE_BIN="$shell_use_bin"
  export SPUR_E2E_ARTIFACTS_DIR="${SPUR_E2E_ARTIFACTS_DIR:-$artifact_root/shell-use}"

  echo "--- shell-use UAT ---"
  echo "shell-use: $shell_use_bin"

  while IFS='|' read -r name fixture script _tape; do
    local path="$SHELL_USE_DIR/journeys/$script"
    if [[ ! -f "$path" ]]; then
      echo "FAIL uat ${name} missing_script=${path}" >&2
      failures=$((failures + 1))
      continue
    fi

    echo
    echo "=== UAT: ${name} (fixture=${fixture}) ==="
    # Fixture is owned by each journey script (or isolate default);
    # we only invoke the existing shell-use owner.
    if SPUR_BIN="$SPUR_BIN" SHELL_USE_BIN="$shell_use_bin" \
      SPUR_E2E_ARTIFACTS_DIR="$SPUR_E2E_ARTIFACTS_DIR" \
      "$path"; then
      echo "PASS uat ${name}"
    else
      local rc=$?
      echo "FAIL uat ${name} exit=${rc}" >&2
      failures=$((failures + 1))
    fi
  done < <(read_journeys)
}

run_capture() {
  echo "--- VHS capture ---"
  if ! "$ROOT/render.sh"; then
    failures=$((failures + 1))
  fi
}

case "$mode" in
  uat)
    run_uat
    ;;
  capture)
    run_capture
    ;;
  all)
    run_uat
    if [[ "$failures" -eq 0 ]]; then
      run_capture
    else
      echo
      echo "skipping capture: UAT had failures" >&2
    fi
    ;;
esac

echo
echo "=== summary ==="
echo "mode:     $mode"
echo "failures: $failures"
if [[ "$failures" -ne 0 ]]; then
  echo "artifacts: $artifact_root" >&2
  exit 1
fi

if [[ "$mode" == "capture" || "$mode" == "all" ]]; then
  echo "media:    $ROOT/out/"
fi
exit 0
