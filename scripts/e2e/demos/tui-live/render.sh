#!/usr/bin/env bash
# VHS media capture against a real SPUR project (navigation-only tapes).
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

echo "SPUR_BIN:         $SPUR_BIN"
echo "SPUR_DEMO_PROJECT:$SPUR_DEMO_PROJECT"
echo

cd "$ROOT"

stems=()
while IFS= read -r stem; do
  [[ -n "$stem" ]] && stems+=("$stem")
done < <(
  awk -F'|' '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    NF >= 3 && $3 != "" { print $3 }
  ' "$ROOT/journeys.conf"
)

status=0
for stem in "${stems[@]}"; do
  tape="tapes/${stem}.tape"
  if [[ ! -f "$tape" ]]; then
    echo "FAIL ${stem} missing_tape=${tape}" >&2
    status=1
    continue
  fi
  echo "==> VHS: ${stem}"
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
exit "$status"
