#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/proof-manifest.json"
HTML="$ROOT/html/index.html"
GRAPH="$ROOT/demo_render/content-graph.json"
failures=0

pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }
require() {
  command -v "$1" >/dev/null || {
    printf 'missing tool: %s\n' "$1" >&2
    exit 2
  }
}

for tool in jq ffprobe rg shasum tesseract; do
  require "$tool"
done

[[ -f "$MANIFEST" ]] && pass "proof manifest exists" || fail "proof manifest exists"
[[ -f "$HTML" ]] && pass "HTML artifact exists" || fail "HTML artifact exists"
if rg -q 'ffmpeg -nostdin -y -v error -ss' "$ROOT/refresh.sh"; then
  pass "publisher protects manifest input from ffmpeg"
else
  fail "publisher protects manifest input from ffmpeg"
fi

if [[ -f "$MANIFEST" ]]; then
  if jq -e '.version == 1 and (.assets | length == 5)' "$MANIFEST" >/dev/null; then
    pass "manifest declares five proof assets"
  else
    fail "manifest declares five proof assets"
  fi

  while IFS=$'\t' read -r id source timestamp checksum; do
    path="$ROOT/$source"
    if [[ ! -f "$path" ]]; then
      fail "$id source exists"
      continue
    fi
    duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$path" 2>/dev/null || true)"
    if [[ -n "$duration" ]] && awk -v t="$timestamp" -v d="$duration" 'BEGIN { exit !(t >= 0 && t < d) }'; then
      pass "$id timestamp in range"
    else
      fail "$id timestamp in range"
    fi
    actual="$(shasum -a 256 "$path" | awk '{print $1}')"
    [[ "$actual" == "$checksum" ]] && pass "$id checksum" || fail "$id checksum"
  done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") |
    [.id,.source,(.timestamp_sec|tostring),.approved_source_sha256] | @tsv' "$MANIFEST")

  if jq -e '([.hero.segments[].id] | sort) == ["plans","resume","session","specialist","workers"]' "$MANIFEST" >/dev/null; then
    pass "hero declares five evidence segments"
  else
    fail "hero declares five evidence segments"
  fi
  if jq -e '. as $root | all(.hero.segments[];
      . as $segment |
      (($segment.proof_terms | length) > 0) and
      any($root.assets[]; .id == $segment.asset_id))' "$MANIFEST" >/dev/null; then
    pass "every hero caption resolves to approved proof"
  else
    fail "every hero caption resolves to approved proof"
  fi
  if [[ -f "$GRAPH" ]] && jq -e --slurpfile manifest "$MANIFEST" \
      '([.segments[] | select(.kind == "video") | .id] | sort) ==
       ([$manifest[0].hero.segments[].id] | sort)' "$GRAPH" >/dev/null; then
    pass "hero graph matches proof manifest"
  else
    fail "hero graph matches proof manifest"
  fi
fi

if [[ -f "$HTML" ]]; then
  if rg -q '<div[^>]+id="(gallery|films)"[^>]*></div>' "$HTML"; then
    fail "proof inventory is not JavaScript-only"
  else
    pass "proof inventory is static"
  fi
  if rg -q 'https?://|<script[^>]+src=|<link[^>]+href=' "$HTML"; then
    fail "HTML has no remote resources"
  else
    pass "HTML has no remote resources"
  fi
  if rg -q '—|–|&mdash;|&#8212;|&ndash;|&#8211;' "$HTML"; then
    fail "artifact has no banned dash glyphs"
  else
    pass "artifact has no banned dash glyphs"
  fi

  while IFS= read -r ref; do
    target="$(dirname "$HTML")/$ref"
    [[ -f "$target" ]] && pass "HTML asset exists: $ref" || fail "HTML asset exists: $ref"
  done < <(rg -o '(src|href)="\.\./[^"#]+' "$HTML" | sed -E 's/^(src|href)="//')
fi

gallery_count=0
for image in "$ROOT"/ph_ready/gallery-*.png; do
  [[ -f "$image" ]] || continue
  gallery_count=$((gallery_count + 1))
  dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$image" 2>/dev/null || true)"
  [[ "$dims" == "1270x760" ]] && pass "$(basename "$image") dimensions" \
    || fail "$(basename "$image") dimensions"
done
[[ "$gallery_count" -eq 5 ]] && pass "exactly five gallery images" || fail "exactly five gallery images"

if [[ -f "$MANIFEST" ]]; then
  while IFS=$'\t' read -r id output term; do
    image="$ROOT/ph_ready/$output"
    if [[ ! -f "$image" ]]; then
      fail "$id published output exists"
      continue
    fi
    ocr="$(tesseract "$image" stdout --psm 11 2>/dev/null | tr '[:lower:]' '[:upper:]')"
    needle="$(printf '%s' "$term" | tr '[:lower:]' '[:upper:]')"
    [[ "$ocr" == *"$needle"* ]] && pass "$id visibly proves $term" \
      || fail "$id visibly proves $term"
  done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") as $a |
    $a.expected_proof[] | [$a.id,$a.output,.] | @tsv' "$MANIFEST")
fi

thumb="$ROOT/ph_ready/thumbnail-240.png"
thumb_spec="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$thumb" 2>/dev/null || true)"
[[ "$thumb_spec" == "240x240" ]] && pass "thumbnail dimensions" || fail "thumbnail dimensions"

hero="$ROOT/ph_ready/hero-video-ph-ready.mp4"
if [[ -f "$hero" ]]; then
  hero_spec="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height -of csv=s=x:p=0 "$hero" 2>/dev/null || true)"
  hero_duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$hero" 2>/dev/null || true)"
  [[ "$hero_spec" == "h264x1920x1080" ]] && pass "hero codec and dimensions" || fail "hero codec and dimensions"
  if [[ -n "$hero_duration" ]] && awk -v d="$hero_duration" 'BEGIN { exit !(d <= 60.0) }'; then
    pass "hero duration"
  else
    fail "hero duration"
  fi
else
  fail "hero exists"
fi

for id in session workers plans specialist resume; do
  [[ -f "$ROOT/demo_render/out/seg-$id.mp4" ]] \
    && pass "hero segment keeps id: $id" || fail "hero segment keeps id: $id"
done

contact_dest="$(mktemp -d "${TMPDIR:-/tmp}/spur-media-contract.XXXXXX")"
if bash "$ROOT/tests/contact-sheet.sh" "$contact_dest" >/dev/null && [[ -f "$contact_dest/gallery-contact.png" ]]; then
  pass "gallery contact sheet"
else
  fail "gallery contact sheet"
fi
rm -rf "$contact_dest"

[[ "$failures" -eq 0 ]] || {
  printf '\n%d media-pack contract check(s) failed\n' "$failures" >&2
  exit 1
}
printf '\nAll media-pack contracts passed\n'
