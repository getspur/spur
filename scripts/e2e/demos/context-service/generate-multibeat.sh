#!/usr/bin/env bash
# Multi-shot marketing film: 4 × 12s Seedance beats → assemble ~48s master.
#
# Mental model (PRODUCT_AND_USAGE storyboard):
#   Beat 1 PROBLEM → Beat 2 SETUP/INDEX → Beat 3 TOOLS → Beat 4 PLANES+CTA
#
# Requires: higgsfield auth, frames from ./render.sh, curl, python3, ffmpeg (or
# Pillow fallback is not used for concat — ffmpeg required for assemble).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRAMES="$ROOT/out/frames"
BEATS_DIR="$ROOT/out/beats"
MASTER="$ROOT/out/marketing-4beat-48s.mp4"
MANIFEST="$BEATS_DIR/manifest.jsonl"

DURATION="${SPUR_DEMO_BEAT_DURATION:-12}"
RESOLUTION="${SPUR_DEMO_RESOLUTION:-1080p}"
ASPECT="${SPUR_DEMO_ASPECT:-16:9}"
WAIT_TIMEOUT="${SPUR_DEMO_WAIT_TIMEOUT:-20m}"

if ! command -v higgsfield >/dev/null 2>&1; then
  echo "error: higgsfield CLI not on PATH" >&2
  exit 1
fi

need=(
  "$FRAMES/01-setup-mid.png"
  "$FRAMES/01-setup-late.png"
  "$FRAMES/02-knowledge-mid.png"
  "$FRAMES/02-read-late.png"
  "$FRAMES/02-cta-end.png"
)
for f in "${need[@]}"; do
  if [[ ! -f "$f" ]]; then
    echo "error: missing $f — run ./render.sh first" >&2
    exit 1
  fi
done

mkdir -p "$BEATS_DIR"
: >"$MANIFEST"

# Continuity bible — same for every beat so identity/style hold across cuts.
STYLE_BIBLE='SPUR Context Service product film. Continuity bible (all beats): dark premium B2B SaaS, Catppuccin-dark terminal aesthetic, cyan and amber accents, soft film grain, 16:9 cinematic, tack sharp, no real third-party brand logos. Optional subtle abstract geometric coding-agent silhouette. Match attached terminal reference images for UI typography and color.'

gen_beat() {
  local id="$1"
  local title="$2"
  local prompt="$3"
  local start_img="$4"
  local end_img="${5:-}"
  local json_out="$BEATS_DIR/${id}.json"
  local mp4_out="$BEATS_DIR/${id}.mp4"

  echo
  echo "==> Beat ${id}: ${title} (${DURATION}s)"

  local -a cmd=(
    higgsfield generate create seedance_2_0
    --prompt "${STYLE_BIBLE}

BEAT ${id} ONLY — ${title}. Full ${DURATION}s dedicated to this beat (do not rush into other beats).

${prompt}"
    --start-image "$start_img"
    --image "$FRAMES/01-setup-mid.png"
    --image "$FRAMES/02-knowledge-mid.png"
    --image "$FRAMES/02-read-late.png"
    --duration "$DURATION"
    --aspect_ratio "$ASPECT"
    --resolution "$RESOLUTION"
    --mode std
    --generate_audio true
    --genre epic
    --wait
    --wait-timeout "$WAIT_TIMEOUT"
    --json
  )
  if [[ -n "$end_img" ]]; then
    cmd+=(--end-image "$end_img")
  fi

  "${cmd[@]}" | tee "$json_out"

  local url
  url="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[0].get("result_url") or "")' "$json_out")"
  if [[ -z "$url" ]]; then
    echo "error: no result_url for beat ${id}" >&2
    exit 1
  fi

  echo "    downloading → ${mp4_out}"
  curl -fsSL "$url" -o "$mp4_out"
  printf '%s\n' "{\"id\":\"${id}\",\"title\":\"${title}\",\"url\":\"${url}\",\"file\":\"${mp4_out}\"}" >>"$MANIFEST"
  printf '%s\n' "$url" >"$BEATS_DIR/${id}.url"
  echo "    ok: $url"
}

# --- 4-beat mental model ----------------------------------------------------

gen_beat 1 "PROBLEM" \
  "Slow push into a dark developer workspace. Dual monitors buried in dependency docs, outdated web search tabs, soft red API-error glow. Hands freeze mid-type. Intercut brief flashes of a real spur context terminal (reference). Mood: agents and humans are strong on in-repo code but weak on dependencies. Tension, no title card yet." \
  "$FRAMES/01-setup-mid.png" \
  "$FRAMES/01-setup-late.png"

gen_beat 2 "SETUP AND INDEX" \
  "Transition from CLI setup (spur context auth / key / mcp checklist vibe from terminal refs) into product magic: a floating package crate labeled serde@1.0.197 opens and unfolds into a luminous 3D code graph of nodes and call edges. Version pin glows green. On-demand indexing of a third-party package revision into a cloud code graph. Hope and clarity rising." \
  "$FRAMES/01-setup-late.png" \
  "$FRAMES/02-knowledge-mid.png"

gen_beat 3 "TOOLS" \
  "Agent multi-round workflow on the external plane. First: external_knowledge_context evidence pack (confidence high, primary_evidence, next selectors) for how Deserialize works in serde@1.0.197. Then carry selector into external_code_read of pkg:serde@1.0.197::Deserialize — real pinned source becomes sharp and readable. UI panels match terminal reference captures. Precision and confidence." \
  "$FRAMES/02-knowledge-mid.png" \
  "$FRAMES/02-read-late.png"

gen_beat 4 "TWO PLANES AND CTA" \
  "Split composition: LEFT local worktree plane (knowledge_context_pack_2 / code_*), RIGHT external dependency plane (external_*). Bright cyan bridge links them. Slow dolly out to title card: SPUR Context Service. Subtitle: Version-precise context for third-party packages. Hold end frame on title. Premium commercial close." \
  "$FRAMES/02-read-late.png" \
  "$FRAMES/02-cta-end.png"

# --- Assemble ----------------------------------------------------------------

echo
echo "==> Assemble 4 beats → ${MASTER}"

CONCAT_LIST="$BEATS_DIR/concat.txt"
{
  printf "file '%s'\n" "$BEATS_DIR/1.mp4"
  printf "file '%s'\n" "$BEATS_DIR/2.mp4"
  printf "file '%s'\n" "$BEATS_DIR/3.mp4"
  printf "file '%s'\n" "$BEATS_DIR/4.mp4"
} >"$CONCAT_LIST"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "error: ffmpeg required to assemble master" >&2
  exit 1
fi

# Re-encode for clean A/V joins (stream copy often fails across gen clips).
ffmpeg -y -nostdin -hide_banner -loglevel error \
  -f concat -safe 0 -i "$CONCAT_LIST" \
  -c:v libx264 -pix_fmt yuv420p -preset fast -crf 18 \
  -c:a aac -b:a 192k -movflags +faststart \
  "$MASTER"

printf '%s\n' "$MASTER" >"$ROOT/out/marketing-4beat-48s.path"
echo
echo "Master: $MASTER"
echo "Beat URLs:"
cat "$BEATS_DIR"/*.url 2>/dev/null || true
echo
ls -la "$BEATS_DIR"/*.mp4 "$MASTER"
