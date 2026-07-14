#!/usr/bin/env bash
# Self-run + capture the LIVE plan-loop seed journey.
#
# Runs problem-plan-loop-drive with SPUR_DEMO_ALLOW_PLAN_LOOP=1, then harvests
# the shell-use asciinema cast into out/ and converts to gif/mp4 when tools allow.
#
# Usage:
#   ./capture-live-seed.sh
#   SPUR_DEMO_PLAN_LOOP_WAIT_S=300 ./capture-live-seed.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_ROOT="$(cd "$ROOT/../.." && pwd)"
# shellcheck disable=SC1091
source "$E2E_ROOT/lib/spur-bin.sh"

OUT="$ROOT/out"
mkdir -p "$OUT"

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN
export SPUR_DEMO_ALLOW_PLAN_LOOP=1
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-240}"
export SHELL_USE_TIMEOUT_MS="${SHELL_USE_TIMEOUT_MS:-180000}"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
stem="14-live-plan-loop-seed-${stamp}"
log="$OUT/${stem}.log"
cast_dest="$OUT/${stem}.cast"

# Snapshot cast mtimes so we can pick the new one after the run
cast_cache="${HOME}/Library/Caches/shell-use"
before_list="$(mktemp)"
if [[ -d "$cast_cache" ]]; then
  find "$cast_cache" -name 'spur-live-problem-plan-loop-drive-*.cast' -type f 2>/dev/null \
    | sort >"$before_list" || true
fi

echo "=== live seed capture ==="
echo "SPUR_BIN:                  $SPUR_BIN"
echo "SPUR_DEMO_ALLOW_PLAN_LOOP: $SPUR_DEMO_ALLOW_PLAN_LOOP"
echo "SPUR_DEMO_PLAN_LOOP_WAIT_S:$SPUR_DEMO_PLAN_LOOP_WAIT_S"
echo "SHELL_USE_TIMEOUT_MS:      $SHELL_USE_TIMEOUT_MS"
echo "log:                       $log"
echo

set +e
(
  cd "$ROOT"
  bash journeys/problem-plan-loop-drive.sh
) 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
set -e

echo
echo "journey exit: $rc"

# Newest cast not in before_list
cast_src=""
if [[ -d "$cast_cache" ]]; then
  while IFS= read -r c; do
    if ! grep -qxF "$c" "$before_list" 2>/dev/null; then
      cast_src="$c"
    fi
  done < <(find "$cast_cache" -name 'spur-live-problem-plan-loop-drive-*.cast' -type f 2>/dev/null | sort)
  # Fallback: newest matching cast by mtime
  if [[ -z "$cast_src" ]]; then
    cast_src="$(find "$cast_cache" -name 'spur-live-problem-plan-loop-drive-*.cast' -type f -print0 2>/dev/null \
      | xargs -0 ls -t 2>/dev/null | head -1 || true)"
  fi
fi
rm -f "$before_list"

if [[ -n "$cast_src" && -f "$cast_src" ]]; then
  cp -p "$cast_src" "$cast_dest"
  echo "cast: $cast_dest ($(wc -c <"$cast_dest") bytes)"
else
  echo "warn: no shell-use cast found under $cast_cache" >&2
fi

# Convert cast → gif/mp4 when possible
gif_out="$OUT/${stem}.gif"
mp4_out="$OUT/${stem}.mp4"

convert_cast() {
  local src="$1"
  if [[ ! -f "$src" ]]; then
    return 1
  fi

  if command -v agg >/dev/null 2>&1; then
    echo "==> agg gif (speed 2.5, idle-limit 1.5s)"
    agg --cols 120 --rows 36 --idle-time-limit 1.5 --speed 2.5 \
      "$src" "$gif_out" || return 1
    echo "gif: $gif_out"
  elif command -v docker >/dev/null 2>&1; then
    echo "==> docker agg (asciinema/agg)"
    docker run --rm -v "$OUT:/data" -v "$(dirname "$src"):/casts:ro" \
      ghcr.io/asciinema/agg:latest \
      --cols 120 --rows 36 --idle-time-limit 1.5 --speed 2.5 \
      "/casts/$(basename "$src")" "/data/$(basename "$gif_out")" \
      && echo "gif: $gif_out" || true
  else
    echo "warn: agg not installed — cast saved; install with: brew install agg" >&2
  fi

  if [[ -f "$gif_out" ]] && command -v ffmpeg >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    echo "==> ffmpeg mp4 via sampled frames (reliable vs raw gif demux)"
    frames="$(mktemp -d "${TMPDIR:-/tmp}/seed-frames.XXXXXX")"
    if python3 - "$gif_out" "$frames" <<'PY'
from PIL import Image
from pathlib import Path
import sys

gif, fd = Path(sys.argv[1]), Path(sys.argv[2])
im = Image.open(gif)
n = getattr(im, "n_frames", 1)
step = max(1, n // 90)
i = idx = 0
while i < 100:
    try:
        im.seek(idx)
    except EOFError:
        break
    im.convert("RGB").resize((960, 540)).save(fd / f"f{i:04d}.jpg", quality=80)
    i += 1
    idx += step
print(f"sampled {i}/{n}")
PY
    then
      if ffmpeg -hide_banner -loglevel error -y -framerate 8 -pattern_type glob \
        -i "$frames/f*.jpg" -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 28 \
        "$mp4_out"; then
        echo "mp4: $mp4_out"
      else
        echo "warn: ffmpeg encode failed" >&2
      fi
    else
      echo "warn: frame sampling failed" >&2
    fi
    rm -rf "$frames"
  fi
}

if [[ -f "$cast_dest" ]]; then
  convert_cast "$cast_dest" || true
fi

# Stable symlink-style copies for latest
if [[ -f "$cast_dest" ]]; then
  cp -p "$cast_dest" "$OUT/14-live-plan-loop-seed.cast"
fi
if [[ -f "$gif_out" ]]; then
  cp -p "$gif_out" "$OUT/14-live-plan-loop-seed.gif"
fi
if [[ -f "$mp4_out" ]]; then
  cp -p "$mp4_out" "$OUT/14-live-plan-loop-seed.mp4"
fi

echo
echo "=== capture summary ==="
ls -la "$OUT"/14-live-plan-loop-seed* "$OUT"/${stem}* 2>/dev/null || ls -la "$OUT" | tail -20
echo "log: $log"
exit "$rc"
