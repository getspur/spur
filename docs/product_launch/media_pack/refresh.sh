#!/usr/bin/env bash
# Refresh media pack from real VHS captures only (no AI mock TUI).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
OUT="$ROOT/scripts/e2e/demos/tui-live/out"
TAPES="$ROOT/scripts/e2e/demos/tui-live/tapes"
PACK="$ROOT/docs/product_launch/media_pack"

mkdir -p "$PACK"/{live_demos,gallery_stills,ph_ready,tapes_index}

copy_demo() {
  local stem="$1"
  for ext in mp4 gif; do
    if [[ -f "$OUT/$stem.$ext" ]]; then
      cp -f "$OUT/$stem.$ext" "$PACK/live_demos/"
    fi
  done
}

for stem in \
  13-problem-plan-loop-drive \
  09-product-e2e-flow \
  10-problem-ops-visibility \
  11-problem-plan-progress \
  12-problem-backlog-triage \
  04-session-resume \
  14-live-plan-loop-seed
do
  copy_demo "$stem"
done

cp -f "$TAPES"/09-product-e2e-flow.tape \
      "$TAPES"/10-problem-ops-visibility.tape \
      "$TAPES"/11-problem-plan-progress.tape \
      "$TAPES"/12-problem-backlog-triage.tape \
      "$TAPES"/13-problem-plan-loop-drive.tape \
      "$TAPES"/04-session-resume.tape \
      "$PACK/tapes_index/" 2>/dev/null || true

pick_best() {
  local src="$1" dest="$2"
  shift 2
  local best="" bestsz=0 t tmp
  for t in "$@"; do
    tmp="$(mktemp -t phframe.XXXXXX).png"
    if ffmpeg -y -ss "$t" -i "$src" -frames:v 1 -q:v 2 "$tmp" 2>/dev/null; then
      local sz
      sz=$(wc -c < "$tmp" | tr -d ' ')
      if (( sz > bestsz )); then
        bestsz=$sz
        rm -f "$best" 2>/dev/null || true
        best=$tmp
      else
        rm -f "$tmp"
      fi
    else
      rm -f "$tmp"
    fi
  done
  [[ -n "$best" ]] || { echo "fail extract $dest from $src" >&2; return 1; }
  mv -f "$best" "$dest"
  echo "still $dest ($bestsz bytes)"
}

pick_best "$OUT/13-problem-plan-loop-drive.mp4" "$PACK/gallery_stills/01-session-plan-loop.png" 8 15 20 28 35 42
pick_best "$OUT/13-problem-plan-loop-drive.mp4" "$PACK/gallery_stills/02-workers-delegate.png" 18 25 32 38 45
pick_best "$OUT/11-problem-plan-progress.mp4" "$PACK/gallery_stills/03-plan-progress.png" 2 5 8 11 14
pick_best "$OUT/09-product-e2e-flow.mp4" "$PACK/gallery_stills/04-explore-cascade.png" 10 20 30 40 50
pick_best "$OUT/04-session-resume.mp4" "$PACK/gallery_stills/05-session-resume.png" 1 2 4 6 8
pick_best "$OUT/12-problem-backlog-triage.mp4" "$PACK/gallery_stills/06-backlog-triage.png" 3 6 10 14 18
pick_best "$OUT/10-problem-ops-visibility.mp4" "$PACK/gallery_stills/07-ops-visibility.png" 5 12 18 25 35
pick_best "$OUT/13-problem-plan-loop-drive.mp4" "$PACK/gallery_stills/00-hero-frame.png" 12 20 28 36

ffmpeg -y -i "$PACK/gallery_stills/00-hero-frame.png" \
  -vf "crop=min(iw\,ih):min(iw\,ih)" \
  "$PACK/ph_ready/_thumb_crop.png" 2>/dev/null
sips -z 512 512 "$PACK/ph_ready/_thumb_crop.png" --out "$PACK/ph_ready/thumbnail-512.png" >/dev/null
sips -z 240 240 "$PACK/ph_ready/_thumb_crop.png" --out "$PACK/ph_ready/thumbnail-240.png" >/dev/null

i=1
for f in 01-session-plan-loop 02-workers-delegate 03-plan-progress 04-explore-cascade 05-session-resume 06-backlog-triage 07-ops-visibility; do
  ffmpeg -y -i "$PACK/gallery_stills/${f}.png" \
    -vf "scale=1270:760:force_original_aspect_ratio=increase,crop=1270:760" \
    "$PACK/ph_ready/gallery-$(printf '%02d' "$i")-${f}-1270x760.png" 2>/dev/null
  i=$((i + 1))
done

cp -f "$OUT/13-problem-plan-loop-drive.mp4" "$PACK/ph_ready/hero-video-plan-loop-drive.mp4"
cp -f "$OUT/13-problem-plan-loop-drive.gif" "$PACK/ph_ready/hero-gif-plan-loop-drive.gif"

echo "Media pack refreshed from $OUT (real captures only)."
echo "Visualizer: $PACK/html/index.html"
