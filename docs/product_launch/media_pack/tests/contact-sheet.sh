#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:?usage: contact-sheet.sh OUTPUT_DIR}"
mkdir -p "$DEST"

for source in "$ROOT"/live_demos/{04-session-resume,09-product-e2e-flow,10-problem-ops-visibility,11-problem-plan-progress,12-problem-backlog-triage,13-problem-plan-loop-drive}.mp4; do
  [[ -f "$source" ]] || {
    printf 'missing capture: %s\n' "$source" >&2
    exit 1
  }
  stem="$(basename "$source" .mp4)"
  duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$source")"
  interval="$(awk -v d="$duration" 'BEGIN { i=d/12; if (i < 1) i=1; printf "%.3f", i }')"
  ffmpeg -y -v error -i "$source" \
    -vf "fps=1/$interval,scale=480:300,tile=4x3:padding=2:margin=2:color=0x0B0E14" \
    -frames:v 1 "$DEST/$stem-contact.png"
done

gallery=(
  "$ROOT/ph_ready/gallery-01-session-detail-1270x760.png"
  "$ROOT/ph_ready/gallery-02-worker-visibility-1270x760.png"
  "$ROOT/ph_ready/gallery-03-plan-state-1270x760.png"
  "$ROOT/ph_ready/gallery-04-specialist-routing-1270x760.png"
  "$ROOT/ph_ready/gallery-05-session-resume-1270x760.png"
)
for image in "${gallery[@]}"; do
  [[ -f "$image" ]] || { printf 'missing gallery image: %s\n' "$image" >&2; exit 1; }
done
ffmpeg -y -v error \
  -i "${gallery[0]}" -i "${gallery[1]}" -i "${gallery[2]}" -i "${gallery[3]}" -i "${gallery[4]}" \
  -filter_complex '[0:v]scale=508:304[a];[1:v]scale=508:304[b];[2:v]scale=508:304[c];[3:v]scale=508:304[d];[4:v]scale=508:304[e];[a][b][c][d][e]xstack=inputs=5:layout=0_0|508_0|1016_0|0_304|508_304[out]' \
  -map '[out]' -frames:v 1 "$DEST/gallery-contact.png"
