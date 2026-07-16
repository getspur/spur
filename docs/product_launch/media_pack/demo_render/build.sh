#!/usr/bin/env bash
# Build the Product Hunt hero from manifest-approved real TUI clips.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACK="$(cd "$ROOT/.." && pwd)"
MANIFEST="$PACK/proof-manifest.json"
OUT="$ROOT/out"
FRAMES="$ROOT/frames"

for tool in ffmpeg ffprobe jq node shasum; do
  command -v "$tool" >/dev/null || { printf 'missing tool: %s\n' "$tool" >&2; exit 2; }
done
[[ -f "$MANIFEST" ]] || { printf 'missing proof manifest: %s\n' "$MANIFEST" >&2; exit 1; }

mkdir -p "$OUT" "$FRAMES"
if [[ ! -d "$ROOT/scripts/node_modules/puppeteer-core" ]]; then
  (cd "$ROOT/scripts" && npm install --ignore-scripts --no-audit --no-fund)
fi
node "$ROOT/scripts/render-html-frames.mjs"

ffmpeg -nostdin -y -v error -loop 1 -i "$FRAMES/01-title.png" -t 3 -r 30 \
  -vf 'scale=1920:1080' -an -c:v libx264 -pix_fmt yuv420p "$OUT/seg-title.mp4"
ffmpeg -nostdin -y -v error -loop 1 -i "$FRAMES/03-end.png" -t 3 -r 30 \
  -vf 'scale=1920:1080' -an -c:v libx264 -pix_fmt yuv420p "$OUT/seg-end.mp4"

segments=()
# Resolve assets without duplicating source facts in the hero section.
while IFS=$'\t' read -r id asset_id start duration caption_frame source checksum; do
  src="$PACK/$source"
  cap="$FRAMES/$caption_frame"
  dest="$OUT/seg-$id.mp4"
  [[ -f "$src" ]] || { printf 'missing hero source for %s: %s\n' "$id" "$src" >&2; exit 1; }
  [[ -f "$cap" ]] || { printf 'missing caption frame for %s: %s\n' "$id" "$cap" >&2; exit 1; }
  actual="$(shasum -a 256 "$src" | awk '{print $1}')"
  [[ "$actual" == "$checksum" ]] || { printf 'hero source checksum drift for %s\n' "$id" >&2; exit 1; }
  source_duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$src")"
  awk -v s="$start" -v d="$duration" -v total="$source_duration" \
    'BEGIN { exit !(s >= 0 && d > 0 && s + d <= total) }' || {
      printf 'hero segment out of range for %s: %s + %s > %s\n' "$id" "$start" "$duration" "$source_duration" >&2
      exit 1
    }
  ffmpeg -nostdin -y -v error -ss "$start" -t "$duration" -i "$src" -i "$cap" \
    -filter_complex '[0:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=0x0B0E14,setsar=1,fps=30[base];[1:v]format=rgba,scale=1920:1080[caption];[base][caption]overlay=0:0[vout]' \
    -map '[vout]' -an -c:v libx264 -pix_fmt yuv420p "$dest"
  segments+=("$dest")
  printf 'built hero segment %s from %s at %ss\n' "$id" "$asset_id" "$start"
done < <(jq -r '.hero.segments[] as $segment |
  .assets as $assets |
  $segment as $s |
  ($assets[] | select(.id == $s.asset_id)) as $asset |
  [$s.id,$s.asset_id,($s.start_sec|tostring),($s.duration_sec|tostring),$s.caption_frame,$asset.source,$asset.approved_source_sha256] | @tsv' "$MANIFEST")

concat="$OUT/concat.txt"
printf "file '%s'\n" "$OUT/seg-title.mp4" > "$concat"
for segment in "${segments[@]}"; do printf "file '%s'\n" "$segment" >> "$concat"; done
printf "file '%s'\n" "$OUT/seg-end.mp4" >> "$concat"

candidate="$OUT/spur-ph-hero-demo.mp4"
ffmpeg -nostdin -y -v error -f concat -safe 0 -i "$concat" -an -c:v libx264 -pix_fmt yuv420p -movflags +faststart "$candidate"
spec="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height -of csv=s=x:p=0 "$candidate")"
total_duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$candidate")"
[[ "$spec" == "h264x1920x1080" ]] || { printf 'wrong hero spec: %s\n' "$spec" >&2; exit 1; }
awk -v d="$total_duration" 'BEGIN { exit !(d <= 60) }' || { printf 'hero exceeds 60 seconds: %s\n' "$total_duration" >&2; exit 1; }

cp -f "$candidate" "$PACK/ph_ready/hero-video-ph-ready.mp4"
printf 'Wrote %s (%ss).\n' "$PACK/ph_ready/hero-video-ph-ready.mp4" "$total_duration"
