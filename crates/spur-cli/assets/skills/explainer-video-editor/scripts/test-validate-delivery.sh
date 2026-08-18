#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
validator="$script_dir/validate-delivery.sh"

if [[ ! -x "$validator" ]]; then
  echo "expected executable validator at $validator" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

video="$tmp_dir/delivery.mp4"
manifest="$tmp_dir/manifest.json"
invalid_gate="$tmp_dir/invalid-gate.json"
invalid_owner="$tmp_dir/invalid-owner.json"
invalid_path="$tmp_dir/invalid-path.json"
invalid_checksum="$tmp_dir/invalid-checksum.json"
invalid_dimensions="$tmp_dir/invalid-dimensions.json"
no_audio_video="$tmp_dir/no-audio.mp4"
no_audio_manifest="$tmp_dir/no-audio.json"
corrupt_video="$tmp_dir/corrupt.mp4"
corrupt_manifest="$tmp_dir/corrupt.json"
non_h264_video="$tmp_dir/non-h264.mp4"
non_h264_manifest="$tmp_dir/non-h264.json"
non_aac_video="$tmp_dir/non-aac.mp4"
non_aac_manifest="$tmp_dir/non-aac.json"

write_matching_manifest() {
  local template="$1"
  local media="$2"
  local output="$3"
  local duration
  local width
  local height
  local frame_rate
  local fps
  local checksum_sha256

  duration="$(
    ffprobe -v error \
      -show_entries format=duration \
      -of default=noprint_wrappers=1:nokey=1 \
      "$media" 2>/dev/null
  )"
  width="$(
    ffprobe -v error \
      -select_streams v:0 \
      -show_entries stream=width \
      -of default=noprint_wrappers=1:nokey=1 \
      "$media" 2>/dev/null
  )"
  height="$(
    ffprobe -v error \
      -select_streams v:0 \
      -show_entries stream=height \
      -of default=noprint_wrappers=1:nokey=1 \
      "$media" 2>/dev/null
  )"
  frame_rate="$(
    ffprobe -v error \
      -select_streams v:0 \
      -show_entries stream=r_frame_rate \
      -of default=noprint_wrappers=1:nokey=1 \
      "$media" 2>/dev/null
  )"
  fps="$(
    awk -v rate="$frame_rate" '
      BEGIN {
        if (split(rate, parts, "/") != 2 || parts[2] + 0 <= 0) {
          exit 1
        }
        printf "%.12f\n", (parts[1] + 0) / (parts[2] + 0)
      }
    '
  )"
  checksum_sha256="$(shasum -a 256 "$media" | awk '{print $1}')"

  jq \
    --arg path "$media" \
    --arg checksum_sha256 "$checksum_sha256" \
    --argjson duration "$duration" \
    --argjson width "$width" \
    --argjson height "$height" \
    --argjson fps "$fps" \
    '
      .delivery.path = $path
      | .delivery.duration_seconds = $duration
      | .delivery.width = $width
      | .delivery.height = $height
      | .delivery.fps = $fps
      | .delivery.checksum_sha256 = $checksum_sha256
    ' \
    "$template" > "$output"
}

assert_stream_codec() {
  local media="$1"
  local stream_selector="$2"
  local expected_codec="$3"
  local actual_codec

  actual_codec="$(
    ffprobe -v error \
      -select_streams "$stream_selector" \
      -show_entries stream=codec_name \
      -of default=noprint_wrappers=1:nokey=1 \
      "$media"
  )"
  if [[ "$actual_codec" != "$expected_codec" ]]; then
    echo "expected $media $stream_selector codec $expected_codec, got $actual_codec" >&2
    exit 1
  fi
}

assert_strictly_readable() {
  local media="$1"

  if ! ffmpeg -v error -xerror -err_detect explode \
    -i "$media" -f null - >/dev/null 2>&1; then
    echo "expected fixture to pass strict full-decode validation: $media" >&2
    exit 1
  fi
}

traceability_rejection_failures=0
traceability_case_number=0

expect_traceability_rejected() {
  local label="$1"
  local filter="$2"
  local candidate

  traceability_case_number=$((traceability_case_number + 1))
  candidate="$tmp_dir/traceability-$traceability_case_number.json"
  jq "$filter" "$manifest" > "$candidate"
  if "$validator" "$candidate" "$video" >/dev/null 2>&1; then
    echo "validator accepted $label" >&2
    traceability_rejection_failures=$((traceability_rejection_failures + 1))
  fi
}

ffmpeg -loglevel error \
  -f lavfi -i "color=c=black:s=320x180:r=30:d=1" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=1" \
  -c:v libx264 -pix_fmt yuv420p \
  -c:a aac -shortest \
  "$video"

checksum_sha256="$(shasum -a 256 "$video" | awk '{print $1}')"

jq -n \
  --arg video "$video" \
  --arg checksum_sha256 "$checksum_sha256" \
  '{
    schema_version: 1,
    project: "validator-fixture",
    route: "create",
    approvals: {
      concept_layout: "approved",
      script_storyboard: "approved",
      paid_generation: "approved"
    },
    claims: [
      {
        claim_id: "claim-01",
        text: "The real source demonstrates the product workflow.",
        source_asset_ids: ["source-doc", "capture-proof"]
      }
    ],
    assets: [
      {
        asset_id: "source-doc",
        owner: "open-design",
        type: "document",
        source_or_job_id: "brief-source",
        approval_status: "approved",
        rights_status: "cleared"
      },
      {
        asset_id: "capture-proof",
        owner: "real-capture",
        type: "video",
        source_or_job_id: "demo-source",
        source_locator: {start_seconds: 0, end_seconds: 0.8},
        approval_status: "approved",
        rights_status: "owned"
      },
      {
        asset_id: "fixture-visual",
        owner: "html-video",
        type: "rendered-video",
        source_or_job_id: "fixture-job",
        approval_status: "approved",
        rights_status: "cleared"
      },
      {
        asset_id: "generated-narration",
        owner: "higgsfield",
        type: "audio",
        source_or_job_id: "voice-job",
        prompt_or_script_revision: "script-r1",
        approval_status: "approved",
        rights_status: "cleared"
      },
      {
        asset_id: "palmier-title",
        owner: "palmier",
        type: "native-text",
        source_or_job_id: "title-clip",
        approval_status: "approved",
        rights_status: "owned"
      }
    ],
    scenes: [
      {
        scene_id: "scene-proof",
        owner: "real-capture",
        timeline_slot: {start_seconds: 0, end_seconds: 0.3},
        asset_ids: ["capture-proof"],
        claim_ids: ["claim-01"]
      },
      {
        scene_id: "scene-plate",
        owner: "html-video",
        timeline_slot: {start_seconds: 0.3, end_seconds: 0.5},
        asset_ids: ["fixture-visual"],
        claim_ids: []
      },
      {
        scene_id: "scene-voice",
        owner: "higgsfield",
        timeline_slot: {start_seconds: 0.5, end_seconds: 0.7},
        asset_ids: ["generated-narration"],
        claim_ids: []
      },
      {
        scene_id: "scene-title",
        owner: "palmier",
        timeline_slot: {start_seconds: 0.7, end_seconds: 1},
        asset_ids: ["palmier-title", "fixture-visual"],
        claim_ids: ["claim-01"]
      }
    ],
    delivery: {
      path: $video,
      duration_seconds: 1,
      width: 320,
      height: 180,
      fps: 30,
      checksum_sha256: $checksum_sha256
    }
  }' > "$manifest"

"$validator" "$manifest" "$video"

expect_traceability_rejected \
  'a fourth approval key' \
  '.approvals.export_review = "approved"'
expect_traceability_rejected \
  'missing claims' \
  'del(.claims)'
expect_traceability_rejected \
  'duplicate claim IDs' \
  '.claims += [.claims[0]]'
expect_traceability_rejected \
  'an unknown claim source asset' \
  '.claims[0].source_asset_ids = ["missing-source"]'
expect_traceability_rejected \
  'an ineligible claim source asset' \
  '.claims[0].source_asset_ids = ["fixture-visual"]'
expect_traceability_rejected \
  'missing scenes' \
  'del(.scenes)'
expect_traceability_rejected \
  'duplicate scene IDs' \
  '.scenes[1].scene_id = .scenes[0].scene_id'
expect_traceability_rejected \
  'an unknown scene owner' \
  '.scenes[0].owner = "open-design"'
expect_traceability_rejected \
  'an invalid scene timeline slot' \
  '.scenes[0].timeline_slot = {start_seconds: 0.4, end_seconds: 0.3}'
expect_traceability_rejected \
  'an unknown scene asset' \
  '.scenes[0].asset_ids = ["missing-asset"]'
expect_traceability_rejected \
  'mixed-owner inputs in a non-Palmier scene' \
  '.scenes[0].asset_ids += ["fixture-visual"]'
expect_traceability_rejected \
  'an open-design asset in a Palmier scene' \
  '.scenes |= map(if .scene_id == "scene-title" then .asset_ids += ["source-doc"] else . end)'
expect_traceability_rejected \
  'an unreferenced non-open-design asset' \
  '.assets += [{asset_id: "orphan-plate", owner: "html-video", type: "video", source_or_job_id: "orphan", approval_status: "approved", rights_status: "cleared"}]'
expect_traceability_rejected \
  'a missing real-capture source locator' \
  '.assets |= map(if .asset_id == "capture-proof" then del(.source_locator) else . end)'
expect_traceability_rejected \
  'an invalid real-capture source locator' \
  '.assets |= map(if .asset_id == "capture-proof" then .source_locator = {start_seconds: 0.5, end_seconds: 0.5} else . end)'
expect_traceability_rejected \
  'a missing Higgsfield prompt or script revision' \
  '.assets |= map(if .asset_id == "generated-narration" then del(.prompt_or_script_revision) else . end)'
expect_traceability_rejected \
  'an unknown scene claim' \
  '.scenes[0].claim_ids = ["missing-claim"]'
expect_traceability_rejected \
  'an unused claim' \
  '.claims += [{claim_id: "unused-claim", text: "Unused factual claim", source_asset_ids: ["source-doc"]}]'

if [[ "$traceability_rejection_failures" -ne 0 ]]; then
  exit 1
fi

ffmpeg -loglevel error \
  -i "$video" \
  -map 0:v:0 -map 0:a:0 \
  -c:v mpeg4 -q:v 5 -c:a copy \
  "$non_h264_video"
assert_stream_codec "$non_h264_video" v:0 mpeg4
assert_stream_codec "$non_h264_video" a:0 aac
assert_strictly_readable "$non_h264_video"
write_matching_manifest "$manifest" "$non_h264_video" "$non_h264_manifest"

ffmpeg -loglevel error \
  -i "$video" \
  -map 0:v:0 -map 0:a:0 \
  -c:v copy -c:a alac \
  "$non_aac_video"
assert_stream_codec "$non_aac_video" v:0 h264
assert_stream_codec "$non_aac_video" a:0 alac
assert_strictly_readable "$non_aac_video"
write_matching_manifest "$manifest" "$non_aac_video" "$non_aac_manifest"

codec_rejection_failures=0
if "$validator" "$non_h264_manifest" "$non_h264_video" >/dev/null 2>&1; then
  echo "expected validator to reject non-H.264 video" >&2
  codec_rejection_failures=$((codec_rejection_failures + 1))
fi
if "$validator" "$non_aac_manifest" "$non_aac_video" >/dev/null 2>&1; then
  echo "expected validator to reject non-AAC audio" >&2
  codec_rejection_failures=$((codec_rejection_failures + 1))
fi
if [[ "$codec_rejection_failures" -ne 0 ]]; then
  exit 1
fi

jq '.approvals.paid_generation = "pending"' "$manifest" > "$invalid_gate"
if "$validator" "$invalid_gate" "$video" >/dev/null 2>&1; then
  echo "expected validator to reject pending paid_generation" >&2
  exit 1
fi

jq '.assets[0].owner = "unknown-editor"' "$manifest" > "$invalid_owner"
if "$validator" "$invalid_owner" "$video" >/dev/null 2>&1; then
  echo "expected validator to reject unknown owner" >&2
  exit 1
fi

jq --arg path "$tmp_dir/not-the-delivery.mp4" \
  '.delivery.path = $path' "$manifest" > "$invalid_path"
if "$validator" "$invalid_path" "$video" >/dev/null 2>&1; then
  echo "expected validator to reject delivery.path mismatch" >&2
  exit 1
fi

jq '.delivery.checksum_sha256 = ("0" * 64)' \
  "$manifest" > "$invalid_checksum"
if "$validator" "$invalid_checksum" "$video" >/dev/null 2>&1; then
  echo "expected validator to reject checksum mismatch" >&2
  exit 1
fi

jq '.delivery.width += 1' "$manifest" > "$invalid_dimensions"
if "$validator" "$invalid_dimensions" "$video" >/dev/null 2>&1; then
  echo "expected validator to reject dimension mismatch" >&2
  exit 1
fi

ffmpeg -loglevel error \
  -i "$video" \
  -map 0:v:0 -c copy -an \
  "$no_audio_video"
write_matching_manifest "$manifest" "$no_audio_video" "$no_audio_manifest"
if "$validator" "$no_audio_manifest" "$no_audio_video" >/dev/null 2>&1; then
  echo "expected validator to reject video without audio" >&2
  exit 1
fi

cp "$video" "$corrupt_video"
key_packet="$(
  ffprobe -v error \
    -select_streams v:0 \
    -show_packets \
    -show_entries packet=pos,size,flags \
    -of json \
    "$corrupt_video" \
    | jq -er '
        [
          .packets[]
          | select((.flags // "") | contains("K"))
          | select((.size | tonumber) > 64)
        ][0]
      '
)"
packet_pos="$(printf '%s\n' "$key_packet" | jq -er '.pos | tonumber')"
packet_size="$(printf '%s\n' "$key_packet" | jq -er '.size | tonumber')"
corrupt_offset=$((packet_pos + packet_size - 32))
printf '\377%.0s' {1..16} \
  | dd of="$corrupt_video" bs=1 seek="$corrupt_offset" count=16 conv=notrunc 2>/dev/null

write_matching_manifest "$manifest" "$corrupt_video" "$corrupt_manifest"
if corrupt_error="$("$validator" "$corrupt_manifest" "$corrupt_video" 2>&1)"; then
  echo "expected validator to reject corrupt video during full decode" >&2
  exit 1
fi
if [[ "$corrupt_error" != *"video fails strict full-decode validation"* ]]; then
  echo "expected corruption to reach the strict full-decode gate" >&2
  exit 1
fi

echo "validate-delivery tests passed"
