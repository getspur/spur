#!/usr/bin/env bash
# Capture Arc A TUI journey demos as mp4/gif via VHS.
# Key sequences and wait strings mirror existing shell-use + vhs goldens.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
REPO_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"
# shellcheck disable=SC1091
source "$ROOT/../geometry.env"

OUT="$ROOT/out"
mkdir -p "$OUT"

if ! command -v vhs >/dev/null 2>&1; then
  echo "error: vhs not on PATH (need charmbracelet vhs + ttyd + ffmpeg)" >&2
  echo "  tip: scripts/e2e/vhs/check-vhs.sh --install" >&2
  exit 1
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "error: ffmpeg not on PATH" >&2
  exit 1
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN

echo "geometry: ${SPUR_VHS_WIDTH}x${SPUR_VHS_HEIGHT} font=${SPUR_VHS_FONT_SIZE} pty=${SPUR_DEMO_COLS}x${SPUR_DEMO_ROWS}"

cd "$ROOT"

# Prefer the e2e pin when check-vhs is available (soft — demos tolerate other vhs).
if [[ -x "$E2E_ROOT/vhs/check-vhs.sh" ]]; then
  if ! "$E2E_ROOT/vhs/check-vhs.sh" >/dev/null 2>&1; then
    echo "warn: vhs version is not the e2e pin (0.11.0); continuing for demo capture" >&2
  fi
fi

stems=()
while IFS= read -r stem; do
  [[ -n "$stem" ]] && stems+=("$stem")
done < <(
  awk -F'|' '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    NF >= 4 && $4 != "" { print $4 }
  ' "$ROOT/journeys.conf"
)

if [[ "${#stems[@]}" -eq 0 ]]; then
  echo "error: no capture tapes listed in journeys.conf" >&2
  exit 1
fi

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
    runtime=$((SECONDS - started))
    echo "PASS ${stem} runtime=${runtime}s"
  else
    rc=$?
    runtime=$((SECONDS - started))
    echo "FAIL ${stem} runtime=${runtime}s vhs_exit=${rc}" >&2
    status=1
  fi
done

echo
echo "Repo:     $REPO_ROOT"
echo "SPUR_BIN: $SPUR_BIN"
echo "Artifacts under $OUT:"
ls -la "$OUT"/*.{mp4,gif} 2>/dev/null || ls -la "$OUT" || true

exit "$status"
