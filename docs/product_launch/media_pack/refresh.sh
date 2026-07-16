#!/usr/bin/env bash
# Publish Product Hunt derivatives from visually approved real TUI evidence.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PACK="$ROOT/docs/product_launch/media_pack"
MANIFEST="$PACK/proof-manifest.json"
TAPES="$ROOT/scripts/e2e/demos/tui-live/tapes"
RENDERER="$PACK/demo_render/scripts/render-html-frames.mjs"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/spur-media-pack.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

require() {
  command -v "$1" >/dev/null || {
    printf 'missing tool: %s\n' "$1" >&2
    exit 2
  }
}

for tool in ffmpeg ffprobe jq node shasum tesseract; do
  require "$tool"
done
[[ -f "$MANIFEST" ]] || { printf 'missing proof manifest: %s\n' "$MANIFEST" >&2; exit 1; }

mkdir -p "$STAGE/gallery_stills" "$STAGE/ph_ready" "$STAGE/tapes_index"

while IFS=$'\t' read -r id source timestamp output checksum x y width height; do
  src="$PACK/$source"
  frame="$STAGE/gallery_stills/$id.png"
  [[ -f "$src" ]] || { printf 'missing source for %s: %s\n' "$id" "$src" >&2; exit 1; }

  actual="$(shasum -a 256 "$src" | awk '{print $1}')"
  [[ "$actual" == "$checksum" ]] || {
    printf 'checksum drift for %s: expected %s, got %s\n' "$id" "$checksum" "$actual" >&2
    exit 1
  }

  read -r source_width source_height duration < <(
    ffprobe -v error -select_streams v:0 \
      -show_entries format=duration:stream=width,height \
      -of default=noprint_wrappers=1:nokey=1 "$src" | paste -sd ' ' -
  )
  awk -v t="$timestamp" -v d="$duration" 'BEGIN { exit !(t >= 0 && t < d) }' || {
    printf 'timestamp out of range for %s: %s >= %s\n' "$id" "$timestamp" "$duration" >&2
    exit 1
  }
  (( x >= 0 && y >= 0 && width > 0 && height > 0 && x + width <= source_width && y + height <= source_height )) || {
    printf 'crop out of range for %s: %sx%s+%s+%s on %sx%s\n' \
      "$id" "$width" "$height" "$x" "$y" "$source_width" "$source_height" >&2
    exit 1
  }

  ffmpeg -y -v error -ss "$timestamp" -i "$src" -frames:v 1 \
    -vf "crop=$width:$height:$x:$y" "$frame"
  printf 'staged %s at %ss from %s\n' "$id" "$timestamp" "$source"
done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") |
  [.id,.source,(.timestamp_sec|tostring),.output,.approved_source_sha256,
   (.crop.x|tostring),(.crop.y|tostring),(.crop.width|tostring),(.crop.height|tostring)] | @tsv' "$MANIFEST")

node "$RENDERER" --gallery-stage "$STAGE"
ffmpeg -y -v error -i "$STAGE/ph_ready/thumbnail-512.png" \
  -vf 'scale=240:240:flags=lanczos' "$STAGE/ph_ready/thumbnail-240.png"

while IFS=$'\t' read -r id output; do
  image="$STAGE/ph_ready/$output"
  dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$image")"
  [[ "$dims" == "1270x760" ]] || { printf 'wrong gallery dimensions for %s: %s\n' "$id" "$dims" >&2; exit 1; }
done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") | [.id,.output] | @tsv' "$MANIFEST")

while IFS=$'\t' read -r id output term; do
  image="$STAGE/ph_ready/$output"
  ocr="$(tesseract "$(realpath "$image")" stdout --psm 11 2>/dev/null | tr '[:lower:]' '[:upper:]')"
  needle="$(printf '%s' "$term" | tr '[:lower:]' '[:upper:]')"
  [[ "$ocr" == *"$needle"* ]] || {
    printf 'visible proof missing for %s: %s\n' "$id" "$term" >&2
    exit 1
  }
done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") as $a |
  $a.expected_proof[] | [$a.id,$a.output,.] | @tsv' "$MANIFEST")

thumb_dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$STAGE/ph_ready/thumbnail-240.png")"
[[ "$thumb_dims" == "240x240" ]] || { printf 'wrong thumbnail dimensions: %s\n' "$thumb_dims" >&2; exit 1; }

cp -p "$TAPES"/04-session-resume.tape \
  "$TAPES"/09-product-e2e-flow.tape \
  "$TAPES"/10-problem-ops-visibility.tape \
  "$TAPES"/11-problem-plan-progress.tape \
  "$TAPES"/12-problem-backlog-triage.tape \
  "$TAPES"/13-problem-plan-loop-drive.tape \
  "$STAGE/tapes_index/"
printf '%s\n' \
  04-session-resume.tape \
  09-product-e2e-flow.tape \
  10-problem-ops-visibility.tape \
  11-problem-plan-progress.tape \
  12-problem-backlog-triage.tape \
  13-problem-plan-loop-drive.tape \
  > "$STAGE/tapes_index/tape_list.txt"

publish_dir() {
  local staged="$1" target="$2" preserve_existing="${3:-0}"
  local next="$target.next.$$" previous="$target.previous.$$"
  rm -rf "$next" "$previous"
  mkdir -p "$next"
  if [[ "$preserve_existing" == "1" && -d "$target" ]]; then
    cp -p "$target"/* "$next/" 2>/dev/null || true
    rm -f "$next"/gallery-*.png "$next"/thumbnail-*.png
  fi
  cp -p "$staged"/* "$next/"
  [[ ! -e "$target" ]] || mv "$target" "$previous"
  mv "$next" "$target"
  rm -rf "$previous"
}

publish_dir "$STAGE/gallery_stills" "$PACK/gallery_stills"
publish_dir "$STAGE/ph_ready" "$PACK/ph_ready" 1
publish_dir "$STAGE/tapes_index" "$PACK/tapes_index"

printf 'Published %s approved Product Hunt gallery assets.\n' \
  "$(jq '[.assets[] | select(.status == "approved" and .kind == "still")] | length' "$MANIFEST")"
printf 'Review surface: %s\n' "$PACK/html/index.html"
