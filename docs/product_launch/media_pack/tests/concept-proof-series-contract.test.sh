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

  if jq -e '
      [.films[] | {id, concept_output, output, takeaway}] == [
        {
          "id": "control-loop",
          "concept_output": "ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4",
          "output": "ph_ready/series/spur-control-loop-proof-40s.mp4",
          "takeaway": "Delegate deeply. Keep the decision."
        },
        {
          "id": "durable-memory",
          "concept_output": "ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4",
          "output": "ph_ready/series/spur-durable-memory-proof-40s.mp4",
          "takeaway": "The agent can stop. The work remains."
        },
        {
          "id": "acp-agents",
          "concept_output": "ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4",
          "output": "ph_ready/series/spur-acp-agents-proof-40s.mp4",
          "takeaway": "Choose the agent. Keep one control system."
        }
      ]
    ' "$MANIFEST" >/dev/null; then
    pass "manifest locks film identities and artifact paths"
  else
    fail "manifest locks film identities and artifact paths"
  fi

  if jq -e '
      .sources["session-detail"].status == "approved"
      and .sources["session-detail"].path == "live_demos/13-problem-plan-loop-drive.mp4"
      and .sources["session-detail"].sha256 == "4d94c2c9d320eb53b4cd4bb56f0bddac337239ee4419e6a3ffc31b47649797d9"
      and .sources["worker-visibility"].status == "approved"
      and .sources["worker-visibility"].path == "live_demos/10-problem-ops-visibility.mp4"
      and .sources["worker-visibility"].sha256 == "4c252847c6498d6be5d7f581c79c0f06665a7fef90f12eb589811fefc207991c"
      and .sources["plan-state"].status == "approved"
      and .sources["plan-state"].path == "live_demos/11-problem-plan-progress.mp4"
      and .sources["plan-state"].sha256 == "011f20addf6850055a9bd062521d22ca94a440898eaf4ec6d5c29c7630335407"
      and .sources["specialist-routing"].status == "approved"
      and .sources["specialist-routing"].path == "live_demos/09-product-e2e-flow.mp4"
      and .sources["specialist-routing"].sha256 == "7fd8473a7870afff7b5085c6a00ef306ac257b0021d8f150884886caa84d47ec"
      and .sources["session-resume"].status == "approved"
      and .sources["session-resume"].path == "live_demos/04-session-resume.mp4"
      and .sources["session-resume"].sha256 == "cb110d2cfa9149cb9d8344987f03f11852a181926ee85a572bebf8dbdff0660c"
    ' "$MANIFEST" >/dev/null; then
    pass "manifest locks approved source identities"
  else
    fail "manifest locks approved source identities"
  fi

  if source_metadata="$(jq -r '
      [
        "session-detail",
        "worker-visibility",
        "plan-state",
        "specialist-routing",
        "session-resume"
      ][] as $source_id |
      [$source_id, .sources[$source_id].path, .sources[$source_id].sha256] | @tsv
    ' "$MANIFEST" 2>/dev/null)"; then
    while IFS=$'\t' read -r source_id source_path expected_sha; do
      if jq -e --arg source_id "$source_id" '
          .sources[$source_id] as $source |
          ($source | type) == "object"
        ' "$MANIFEST" >/dev/null; then
        pass "$source_id source is keyed"
      else
        fail "$source_id source is keyed"
        continue
      fi

      approved_source="$ROOT/$source_path"
      if [[ -f "$approved_source" ]]; then
        pass "$source_id source exists"
      else
        fail "$source_id source exists"
        continue
      fi

      if ! actual_sha="$(shasum -a 256 "$approved_source" | awk '{print $1}')"; then
        fail "$source_id source checksum is readable"
        continue
      fi
      if [[ -n "$expected_sha" && "$actual_sha" == "$expected_sha" ]]; then
        pass "$source_id checksum"
      else
        fail "$source_id checksum"
      fi
    done <<<"$source_metadata"
  else
    fail "series source metadata readable"
  fi

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
  notebook_source_ok=false
  notebook_html_ok=false
  if notebook_source="$(jq -r '
      .cells[].source | if type == "array" then join("") else . end
    ' "$NOTEBOOK" 2>/dev/null)"; then
    notebook_source_ok=true
    pass "notebook source JSON is readable"
  else
    notebook_source=""
    fail "notebook source JSON is readable"
  fi
  if notebook_html="$(jq -r '
      .cells[].outputs[]? | .data["text/html"]? // empty |
      if type == "array" then join("") else . end
    ' "$NOTEBOOK" 2>/dev/null)"; then
    notebook_html_ok=true
    pass "notebook HTML output JSON is readable"
  else
    notebook_html=""
    fail "notebook HTML output JSON is readable"
  fi

  for required in \
    'Delegate deeply. Keep the decision.' \
    'The agent can stop. The work remains.' \
    'Choose the agent. Keep one control system.' \
    'INSTALL SPUR · COMMUNITY FREE'; do
    if [[ "$notebook_source_ok" == true ]]; then
      if [[ "$notebook_source" == *"$required"* ]]; then
        pass "notebook source copy: $required"
      else
        fail "notebook source copy: $required"
      fi
    fi
    if [[ "$notebook_html_ok" == true ]]; then
      if [[ "$notebook_html" == *"$required"* ]]; then
        pass "rendered notebook copy: $required"
      else
        fail "rendered notebook copy: $required"
      fi
    fi
  done
else
  fail "concept-proof series notebook exists"
fi

if rg -qi --hidden --glob '!**/tests/**' 'otobank' "$ROOT"; then
  leak_scan_status=0
else
  leak_scan_status=$?
fi
case "$leak_scan_status" in
  0) fail "SPUR concept-proof series contains no unrelated Otobank copy" ;;
  1) pass "SPUR concept-proof series contains no unrelated Otobank copy" ;;
  *) fail "SPUR concept-proof series Otobank scan completes" ;;
esac

if [[ -f "$MANIFEST" ]]; then
  if film_outputs="$(jq -r '
      .films[] | [.id, .output, (.duration_frames | tostring)] | @tsv
    ' "$MANIFEST" 2>/dev/null)"; then
    if [[ -n "$film_outputs" ]]; then
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

        if ffmpeg -nostdin -xerror -err_detect explode -v error -i "$film" -f null - >/dev/null 2>&1; then
          pass "$film_id full decode"
        else
          fail "$film_id full decode"
        fi
      done <<<"$film_outputs"
    fi
  else
    fail "series film outputs readable"
  fi
fi

[[ "$failures" -eq 0 ]] || {
  printf '\n%d concept-proof series contract check(s) failed\n' "$failures" >&2
  exit 1
}
printf '\nAll concept-proof series contracts passed\n'
