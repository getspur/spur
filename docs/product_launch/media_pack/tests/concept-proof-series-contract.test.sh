#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/concept-proof-series-manifest.json"
NOTEBOOK="$ROOT/product-hunt-media-pack.ipynb"
failures=0

pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }
require() {
  command -v "$1" >/dev/null || {
    printf 'missing tool: %s\n' "$1" >&2
    exit 2
  }
}

for tool in jq ffprobe ffmpeg rg shasum awk; do
  require "$tool"
done

if [[ -f "$MANIFEST" ]]; then
  pass "concept-proof series manifest exists"
else
  fail "concept-proof series manifest exists"
fi

if [[ -f "$MANIFEST" ]]; then
  if jq -e '
      .version == 1
      and .fps == 30
      and .canvas == {"width": 1920, "height": 1080}
      and (.films | type == "array" and length == 3)
      and all(.films[];
        .duration_seconds == 40
        and .duration_frames == 1200
        and .chapters == {
          "hook": [0, 90],
          "concept": [90, 390],
          "match": [390, 480],
          "proof": [480, 1050],
          "end": [1050, 1200]
        }
        and ([.proof_sources[].duration_seconds] | add) == 19
      )
    ' "$MANIFEST" >/dev/null; then
    pass "manifest locks the three-film 40-second series structure"
  else
    fail "manifest locks the three-film 40-second series structure"
  fi

  for source_id in \
    session-detail \
    worker-visibility \
    plan-state \
    specialist-routing \
    session-resume; do
    if jq -e --arg source_id "$source_id" '
        .sources[$source_id] as $source |
        ($source | type) == "object"
      ' "$MANIFEST" >/dev/null; then
      pass "$source_id source is keyed"
    else
      fail "$source_id source is keyed"
      continue
    fi

    IFS=$'\t' read -r source_path expected_sha < <(
      jq -r --arg source_id "$source_id" \
        '.sources[$source_id] | [.path, .sha256] | @tsv' "$MANIFEST"
    )
    approved_source="$ROOT/$source_path"
    if [[ -f "$approved_source" ]]; then
      pass "$source_id source exists"
    else
      fail "$source_id source exists"
      continue
    fi

    actual_sha="$(shasum -a 256 "$approved_source" | awk '{print $1}')"
    if [[ -n "$expected_sha" && "$actual_sha" == "$expected_sha" ]]; then
      pass "$source_id checksum"
    else
      fail "$source_id checksum"
    fi
  done

  diagnostic_sha="b5c407a3753bae990b0cdf95fd5dac2c747934e15f8a314aaff42e52bf83ecb5"
  if jq -e --arg diagnostic_sha "$diagnostic_sha" '
      .sources["four-agent-diagnostic"] as $source |
      ($source | type) == "object"
      and $source.status == "diagnostic"
      and $source.sha256 == $diagnostic_sha
    ' "$MANIFEST" >/dev/null; then
    pass "four-agent diagnostic status and checksum are locked"
  else
    fail "four-agent diagnostic status and checksum are locked"
  fi

  if jq -e '
      all(.films[].proof_sources[]?;
        .source_id != "four-agent-diagnostic" or .watermark == true
      )
    ' "$MANIFEST" >/dev/null; then
    pass "four-agent diagnostic usage is watermarked"
  else
    fail "four-agent diagnostic usage is watermarked"
  fi
fi

if [[ -f "$NOTEBOOK" ]]; then
  notebook_source="$(jq -r '
    .cells[].source | if type == "array" then join("") else . end
  ' "$NOTEBOOK")"
  notebook_html="$(jq -r '
    .cells[].outputs[]? | .data["text/html"]? // empty |
    if type == "array" then join("") else . end
  ' "$NOTEBOOK")"

  for required in \
    'Delegate deeply. Keep the decision.' \
    'The agent can stop. The work remains.' \
    'Choose the agent. Keep one control system.' \
    'INSTALL SPUR · COMMUNITY FREE'; do
    if [[ "$notebook_source" == *"$required"* ]]; then
      pass "notebook source copy: $required"
    else
      fail "notebook source copy: $required"
    fi
    if [[ "$notebook_html" == *"$required"* ]]; then
      pass "rendered notebook copy: $required"
    else
      fail "rendered notebook copy: $required"
    fi
  done
else
  fail "concept-proof series notebook exists"
fi

if rg -qi --hidden \
    --glob '!concept-proof-series-contract.test.sh' \
    'otobank' "$ROOT"; then
  fail "SPUR concept-proof series contains no unrelated Otobank copy"
else
  pass "SPUR concept-proof series contains no unrelated Otobank copy"
fi

if [[ -f "$MANIFEST" ]]; then
  while IFS=$'\t' read -r film_id output expected_frames; do
    film="$ROOT/$output"
    if [[ ! -f "$film" ]]; then
      fail "$film_id output exists"
      continue
    fi

    video_spec="$(ffprobe -v error -select_streams v:0 \
      -show_entries stream=codec_name,width,height,r_frame_rate,avg_frame_rate,nb_frames \
      -of json "$film" 2>/dev/null || true)"
    if [[ -n "$video_spec" ]] && jq -e --argjson expected_frames "$expected_frames" '
        .streams[0].codec_name == "h264"
        and .streams[0].width == 1920
        and .streams[0].height == 1080
        and .streams[0].r_frame_rate == "30/1"
        and .streams[0].avg_frame_rate == "30/1"
        and (.streams[0].nb_frames | tonumber) == $expected_frames
      ' <<<"$video_spec" >/dev/null; then
      pass "$film_id H.264 1920x1080 30fps frame contract"
    else
      fail "$film_id H.264 1920x1080 30fps frame contract"
    fi

    audio_spec="$(ffprobe -v error -select_streams a:0 \
      -show_entries stream=codec_name,sample_rate,channels \
      -of json "$film" 2>/dev/null || true)"
    if [[ -n "$audio_spec" ]] && jq -e '
        .streams[0].codec_name == "aac"
        and .streams[0].sample_rate == "48000"
        and .streams[0].channels == 2
      ' <<<"$audio_spec" >/dev/null; then
      pass "$film_id AAC 48k stereo contract"
    else
      fail "$film_id AAC 48k stereo contract"
    fi

    duration="$(ffprobe -v error -show_entries format=duration \
      -of default=noprint_wrappers=1:nokey=1 "$film" 2>/dev/null || true)"
    if [[ "$duration" == "40.000000" ]]; then
      pass "$film_id exact duration"
    else
      fail "$film_id exact duration"
    fi

    if ffmpeg -nostdin -v error -i "$film" -f null - >/dev/null 2>&1; then
      pass "$film_id full decode"
    else
      fail "$film_id full decode"
    fi
  done < <(jq -r '.films[] | [.id, .output, (.duration_frames | tostring)] | @tsv' "$MANIFEST")
fi

[[ "$failures" -eq 0 ]] || {
  printf '\n%d concept-proof series contract check(s) failed\n' "$failures" >&2
  exit 1
}
printf '\nAll concept-proof series contracts passed\n'
