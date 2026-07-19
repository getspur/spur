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
    assets: [
      {
        asset_id: "fixture-visual",
        owner: "html-video",
        type: "rendered-video",
        source_or_job_id: "fixture-job",
        approval_status: "approved",
        rights_status: "cleared"
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

echo "validate-delivery tests passed"
