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
# shellcheck disable=SC1091
source "$ROOT/../geometry.env"

OUT="$ROOT/out"
mkdir -p "$OUT"

if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" != "1" ]]; then
  export SPUR_DEMO_ALLOW_PLAN_LOOP=1
fi
export SPUR_DEMO_ALLOW_PLAN_LOOP="${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}"
export SPUR_DEMO_ALLOW_HITL_LOOP="${SPUR_DEMO_ALLOW_HITL_LOOP:-0}"
export SPUR_DEMO_CAPTURE_STEM_PREFIX="${SPUR_DEMO_CAPTURE_STEM_PREFIX:-14-live-plan-loop-seed}"
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-240}"
export SHELL_USE_TIMEOUT_MS="${SHELL_USE_TIMEOUT_MS:-180000}"
# Film pacing for seed (readable high-res story; UAT leaves this unset/0)
export SPUR_DEMO_STORY_PACE="${SPUR_DEMO_STORY_PACE:-1}"
# Story-friendly cast speed unless caller overrides
export SPUR_AGG_SPEED="${SPUR_AGG_SPEED:-1.15}"

if [[ -n "${SPUR_DEMO_PROJECT:-}" ]]; then
  capture_project="$SPUR_DEMO_PROJECT"
else
  capture_project="$(git -C "$E2E_ROOT/../.." rev-parse --show-toplevel)"
fi

if [[ "$SPUR_DEMO_ALLOW_HITL_LOOP" == "1" && ! -d "$capture_project/.beads" ]]; then
  cat >&2 <<EOF
D4 requires a beads-backed project before TUI startup; capture aborted.
missing: $capture_project/.beads

SPUR_DEMO_PROJECT=/path/to/beads-project selects an initialized beads-backed project.
Alternatively, initialize beads in the effective project.
No D4 TUI, brain, or worker spend was started.
EOF
  exit 2
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  exit 1
fi
export SPUR_BIN

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
stem_prefix="$SPUR_DEMO_CAPTURE_STEM_PREFIX"
stem="${stem_prefix}-${stamp}"
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
echo "SPUR_DEMO_ALLOW_HITL_LOOP: $SPUR_DEMO_ALLOW_HITL_LOOP"
echo "effective project:          $capture_project"
echo "SPUR_DEMO_PLAN_LOOP_WAIT_S:$SPUR_DEMO_PLAN_LOOP_WAIT_S"
echo "stable output stem:         $stem_prefix"
echo "SHELL_USE_TIMEOUT_MS:      $SHELL_USE_TIMEOUT_MS"
echo "geometry:                  ${SPUR_VHS_WIDTH}x${SPUR_VHS_HEIGHT} font=${SPUR_VHS_FONT_SIZE} pty=${SPUR_DEMO_COLS}x${SPUR_DEMO_ROWS}"
echo "story_pace:                ${SPUR_DEMO_STORY_PACE} agg_speed=${SPUR_AGG_SPEED}"
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
cp -p "$log" "$OUT/${stem_prefix}.log"

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

  local agg_cols="${SPUR_AGG_COLS:-200}"
  local agg_rows="${SPUR_AGG_ROWS:-50}"
  local agg_speed="${SPUR_AGG_SPEED:-2.5}"
  local agg_idle="${SPUR_AGG_IDLE_LIMIT:-1.5}"
  # Preview encode width (full Air-native gifs are huge; mp4 samples to 1920)
  local preview_w="${SPUR_CAPTURE_PREVIEW_WIDTH:-1920}"

  if command -v agg >/dev/null 2>&1; then
    echo "==> agg gif (cols=${agg_cols} rows=${agg_rows} speed=${agg_speed})"
    agg --cols "$agg_cols" --rows "$agg_rows" \
      --idle-time-limit "$agg_idle" --speed "$agg_speed" \
      "$src" "$gif_out" || return 1
    echo "gif: $gif_out"
  elif command -v docker >/dev/null 2>&1; then
    echo "==> docker agg (asciinema/agg)"
    docker run --rm -v "$OUT:/data" -v "$(dirname "$src"):/casts:ro" \
      ghcr.io/asciinema/agg:latest \
      --cols "$agg_cols" --rows "$agg_rows" \
      --idle-time-limit "$agg_idle" --speed "$agg_speed" \
      "/casts/$(basename "$src")" "/data/$(basename "$gif_out")" \
      && echo "gif: $gif_out" || true
  else
    echo "warn: agg not installed — cast saved; install with: brew install agg" >&2
  fi

  if [[ -f "$gif_out" ]] && command -v ffmpeg >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    echo "==> ffmpeg mp4 via sampled frames (preview width ${preview_w})"
    frames="$(mktemp -d "${TMPDIR:-/tmp}/seed-frames.XXXXXX")"
    if python3 - "$gif_out" "$frames" "$preview_w" <<'PY'
from PIL import Image
from pathlib import Path
import sys

gif, fd = Path(sys.argv[1]), Path(sys.argv[2])
target_w = int(sys.argv[3])
im = Image.open(gif)
n = getattr(im, "n_frames", 1)
step = max(1, n // 120)
i = idx = 0
while i < 140:
    try:
        im.seek(idx)
    except EOFError:
        break
    frame = im.convert("RGB")
    w, h = frame.size
    if w > target_w:
        nh = max(2, int(h * (target_w / w)))
        nh -= nh % 2
        frame = frame.resize((target_w, nh))
    frame.save(fd / f"f{i:04d}.jpg", quality=85)
    i += 1
    idx += step
print(f"sampled {i}/{n} preview_w={target_w}")
PY
    then
      if ffmpeg -hide_banner -loglevel error -y -framerate 10 -pattern_type glob \
        -i "$frames/f*.jpg" -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 26 \
        -movflags +faststart "$mp4_out"; then
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
  cp -p "$cast_dest" "$OUT/${stem_prefix}.cast"
fi
if [[ -f "$gif_out" ]]; then
  cp -p "$gif_out" "$OUT/${stem_prefix}.gif"
fi
if [[ -f "$mp4_out" ]]; then
  cp -p "$mp4_out" "$OUT/${stem_prefix}.mp4"
fi

echo
echo "=== capture summary ==="
ls -la "$OUT"/${stem_prefix}* "$OUT"/${stem}* 2>/dev/null || ls -la "$OUT" | tail -20
echo "log: $log"
exit "$rc"
