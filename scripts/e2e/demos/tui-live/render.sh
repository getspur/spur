#!/usr/bin/env bash
# VHS media capture against a real SPUR project.
# agent-send tape runs only when SPUR_DEMO_ALLOW_AGENT_SEND=1.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"

OUT="$ROOT/out"
mkdir -p "$OUT"

if ! command -v vhs >/dev/null 2>&1; then
  echo "error: vhs not on PATH (need vhs + ttyd + ffmpeg)" >&2
  exit 1
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN
export SPUR_DEMO_PROJECT="${SPUR_DEMO_PROJECT:-$(git -C "$E2E_ROOT/../.." rev-parse --show-toplevel)}"

allow_send="${SPUR_DEMO_ALLOW_AGENT_SEND:-0}"

echo "SPUR_BIN:                  $SPUR_BIN"
echo "SPUR_DEMO_PROJECT:         $SPUR_DEMO_PROJECT"
echo "SPUR_DEMO_ALLOW_AGENT_SEND:$allow_send"
echo

cd "$ROOT"

# name|script|tape|flags
rows=()
while IFS= read -r row; do
  [[ -n "$row" ]] && rows+=("$row")
done < <(
  awk -F'|' '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    NF >= 3 && $3 != "" {
      flags = (NF >= 4 ? $4 : "")
      print $1 "|" $3 "|" flags
    }
  ' "$ROOT/journeys.conf"
)

status=0
skipped=0
for row in "${rows[@]}"; do
  IFS='|' read -r name stem flags <<<"$row"
  if [[ "$flags" == *agent-send* && "$allow_send" != "1" ]]; then
    echo "SKIP ${stem} (set SPUR_DEMO_ALLOW_AGENT_SEND=1 to capture agent-send)"
    skipped=$((skipped + 1))
    continue
  fi

  tape="tapes/${stem}.tape"
  if [[ ! -f "$tape" ]]; then
    echo "FAIL ${stem} missing_tape=${tape}" >&2
    status=1
    continue
  fi
  echo "==> VHS: ${stem} (${name})"
  started=$SECONDS
  if vhs -q "$tape"; then
    echo "PASS ${stem} runtime=$((SECONDS - started))s"
  else
    rc=$?
    echo "FAIL ${stem} runtime=$((SECONDS - started))s vhs_exit=${rc}" >&2
    status=1
  fi
done

echo
echo "Artifacts under $OUT:"
ls -la "$OUT"/*.{mp4,gif} 2>/dev/null || ls -la "$OUT" || true
echo "skipped_agent_send_gated: $skipped"
exit "$status"
