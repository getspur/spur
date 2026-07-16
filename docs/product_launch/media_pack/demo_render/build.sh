#!/usr/bin/env bash
# Build revised PH hero demo from real VHS + HTML frames.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACK="$(cd "$ROOT/.." && pwd)"
SRC="$PACK/ph_ready/hero-video-plan-loop-drive.mp4"
cd "$ROOT"
mkdir -p frames out scripts

if [[ ! -f "$SRC" ]]; then
  echo "missing $SRC — run ../refresh.sh first" >&2
  exit 1
fi

# deps
if [[ ! -d scripts/node_modules/puppeteer-core ]]; then
  (cd scripts && npm init -y >/dev/null 2>&1 && npm install puppeteer-core --no-fund --no-audit)
fi

node scripts/render-html-frames.mjs

ffmpeg -y -loop 1 -i frames/01-title.png -t 3 -r 30 -pix_fmt yuv420p -c:v libx264 \
  -vf "scale=1920:1080" out/seg-title.mp4
ffmpeg -y -loop 1 -i frames/03-end.png -t 3 -r 30 -pix_fmt yuv420p -c:v libx264 \
  -vf "scale=1920:1080" out/seg-end.mp4

ffmpeg -y -ss 5 -to 40 -i "$SRC" \
  -i frames/cap-session.png \
  -i frames/cap-workers.png \
  -i frames/cap-plans.png \
  -i frames/cap-specialists.png \
  -i frames/cap-resume.png \
  -filter_complex "\
[0:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=0x0B0E14,setsar=1,fps=30[base];\
[1:v]format=rgba,scale=1920:1080[c0];\
[2:v]format=rgba,scale=1920:1080[c1];\
[3:v]format=rgba,scale=1920:1080[c2];\
[4:v]format=rgba,scale=1920:1080[c3];\
[5:v]format=rgba,scale=1920:1080[c4];\
[base][c0]overlay=0:0:enable='between(t,0,5)'[v1];\
[v1][c1]overlay=0:0:enable='between(t,8,14)'[v2];\
[v2][c2]overlay=0:0:enable='between(t,16,22)'[v3];\
[v3][c3]overlay=0:0:enable='between(t,24,30)'[v4];\
[v4][c4]overlay=0:0:enable='between(t,30,35)'[vout]\
" -map "[vout]" -an -c:v libx264 -pix_fmt yuv420p out/seg-demo.mp4

printf "file '%s'\n" "$ROOT/out/seg-title.mp4" "$ROOT/out/seg-demo.mp4" "$ROOT/out/seg-end.mp4" > out/concat.txt
ffmpeg -y -f concat -safe 0 -i out/concat.txt -c:v libx264 -pix_fmt yuv420p -movflags +faststart \
  out/spur-ph-hero-demo.mp4

cp -f out/spur-ph-hero-demo.mp4 "$PACK/ph_ready/hero-video-ph-ready.mp4"
ffmpeg -y -i out/spur-ph-hero-demo.mp4 -vf "fps=8,scale=960:-1" -t 15 out/spur-ph-hero-demo-preview.gif 2>/dev/null || true

echo "Wrote $PACK/ph_ready/hero-video-ph-ready.mp4"
ffprobe -v error -show_entries format=duration -show_entries stream=width,height -of default=nw=1 \
  "$PACK/ph_ready/hero-video-ph-ready.mp4"
