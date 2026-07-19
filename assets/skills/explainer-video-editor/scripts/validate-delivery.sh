#!/usr/bin/env bash
set -euo pipefail

usage='usage: validate-delivery.sh MANIFEST.json VIDEO.mp4'

if [[ "$#" -ne 2 ]]; then
  printf '%s\n' "$usage" >&2
  exit 2
fi

manifest="$1"
video="$2"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

for dependency in jq ffprobe ffmpeg shasum awk; do
  if ! command -v "$dependency" >/dev/null 2>&1; then
    fail "required command not found: $dependency"
  fi
done

if [[ ! -f "$manifest" ]]; then
  fail "manifest file not found: $manifest"
fi

if [[ ! -f "$video" ]]; then
  fail "video file not found: $video"
fi

if ! jq -e '
  .schema_version == 1
  and (.project | type == "string" and length > 0)
  and (.route == "create" or .route == "enhance")
  and (.approvals.concept_layout == "approved")
  and (.approvals.script_storyboard == "approved")
  and (.approvals.paid_generation == "approved")
  and (.assets | type == "array" and length > 0)
  and all(.assets[];
    (.asset_id | type == "string" and length > 0)
    and (
      .owner == "open-design"
      or .owner == "html-video"
      or .owner == "higgsfield"
      or .owner == "palmier"
      or .owner == "real-capture"
    )
    and (.type | type == "string" and length > 0)
    and (.source_or_job_id | type == "string" and length > 0)
    and (.approval_status == "approved")
    and (.rights_status == "cleared" or .rights_status == "owned")
  )
  and (
    [.assets[].asset_id] as $asset_ids
    | ($asset_ids | length) == ($asset_ids | unique | length)
  )
  and (.delivery.path | type == "string" and length > 0)
  and (.delivery.duration_seconds | type == "number" and . > 0)
  and (.delivery.width | type == "number" and . > 0)
  and (.delivery.height | type == "number" and . > 0)
  and (.delivery.fps | type == "number" and . > 0)
  and (
    .delivery.checksum
    | type == "string" and test("^[0-9a-f]{64}$")
  )
' "$manifest" >/dev/null 2>&1; then
  fail 'manifest violates the explainer delivery contract'
fi

expected_video="$(jq -r '.delivery.path' "$manifest")"
if [[ "$expected_video" != "$video" ]]; then
  fail 'delivery.path does not match the supplied video path'
fi

expected_duration="$(jq -r '.delivery.duration_seconds' "$manifest")"
expected_width="$(jq -r '.delivery.width' "$manifest")"
expected_height="$(jq -r '.delivery.height' "$manifest")"
expected_fps="$(jq -r '.delivery.fps' "$manifest")"
expected_checksum="$(jq -r '.delivery.checksum' "$manifest")"

if ! probe_json="$(
  ffprobe -v error \
    -show_entries 'format=duration:stream=codec_type,codec_name,width,height,r_frame_rate' \
    -of json \
    "$video"
)"; then
  fail 'video is not readable by ffprobe'
fi

if ! actual_duration="$(
  printf '%s\n' "$probe_json" \
    | jq -er '.format.duration | tonumber | select(. > 0)'
)"; then
  fail 'video has no readable positive duration'
fi

if ! actual_width="$(
  printf '%s\n' "$probe_json" \
    | jq -er '[.streams[]? | select(.codec_type == "video")][0].width | select(type == "number" and . > 0)'
)"; then
  fail 'video has no readable video stream width'
fi

if ! actual_height="$(
  printf '%s\n' "$probe_json" \
    | jq -er '[.streams[]? | select(.codec_type == "video")][0].height | select(type == "number" and . > 0)'
)"; then
  fail 'video has no readable video stream height'
fi

if ! frame_rate="$(
  printf '%s\n' "$probe_json" \
    | jq -er '[.streams[]? | select(.codec_type == "video")][0].r_frame_rate | select(type == "string" and length > 0)'
)"; then
  fail 'video has no readable video stream frame rate'
fi

if ! audio_codec="$(
  printf '%s\n' "$probe_json" \
    | jq -er '[.streams[]? | select(.codec_type == "audio")][0].codec_name | select(type == "string" and length > 0)'
)"; then
  fail 'video has no readable audio stream'
fi

if ! actual_fps="$(
  awk -v rate="$frame_rate" '
    BEGIN {
      part_count = split(rate, parts, "/")
      if (part_count == 1) {
        numerator = parts[1] + 0
        denominator = 1
      } else if (part_count == 2) {
        numerator = parts[1] + 0
        denominator = parts[2] + 0
      } else {
        exit 1
      }

      if (numerator <= 0 || denominator <= 0) {
        exit 1
      }

      printf "%.12f\n", numerator / denominator
    }
  '
)"; then
  fail 'video has an invalid video stream frame rate'
fi

if ! awk -v expected="$expected_width" -v actual="$actual_width" \
  'BEGIN { exit !(actual == expected) }'; then
  fail 'video width does not match the manifest'
fi

if ! awk -v expected="$expected_height" -v actual="$actual_height" \
  'BEGIN { exit !(actual == expected) }'; then
  fail 'video height does not match the manifest'
fi

if ! awk -v expected="$expected_duration" -v actual="$actual_duration" '
  BEGIN {
    difference = actual - expected
    if (difference < 0) {
      difference = -difference
    }
    exit !(difference <= 0.05)
  }
'; then
  fail 'video duration differs from the manifest by more than 0.05 seconds'
fi

if ! awk -v expected="$expected_fps" -v actual="$actual_fps" '
  BEGIN {
    difference = actual - expected
    if (difference < 0) {
      difference = -difference
    }
    exit !(difference <= 0.001)
  }
'; then
  fail 'video frame rate differs from the manifest by more than 0.001 fps'
fi

actual_checksum="$(shasum -a 256 "$video" | awk '{print $1}')"
if [[ "$actual_checksum" != "$expected_checksum" ]]; then
  fail 'video checksum does not match the manifest'
fi

if ! ffmpeg -v error -err_detect explode -i "$video" -f null -; then
  fail 'video fails strict full-decode validation'
fi

jq -n \
  --arg status 'ok' \
  --arg video "$video" \
  --argjson duration_seconds "$actual_duration" \
  --argjson width "$actual_width" \
  --argjson height "$actual_height" \
  --argjson fps "$actual_fps" \
  --arg audio_codec "$audio_codec" \
  --arg checksum_sha256 "$actual_checksum" \
  '{
    status: $status,
    video: $video,
    duration_seconds: $duration_seconds,
    width: $width,
    height: $height,
    fps: $fps,
    audio_codec: $audio_codec,
    checksum_sha256: $checksum_sha256
  }'
