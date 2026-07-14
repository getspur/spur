#!/usr/bin/env bash
# Seedance 2.0 marketing film: 4-beat storyboard grounded on VHS terminal refs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRAMES="$ROOT/out/frames"
OUT_JSON="$ROOT/out/higgsfield-result.json"

if ! command -v higgsfield >/dev/null 2>&1; then
  echo "error: higgsfield CLI not on PATH" >&2
  exit 1
fi

need=(
  "$FRAMES/01-setup-mid.png"
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

PROMPT='Marketing film for SPUR Context Service. Ground visual style on the attached real terminal captures (dark Catppuccin CLI, cyan accents). Multi-shot 4-beat product storyboard. Abstract geometric coding agent optional and subtle.

BEAT 1 (0-3s) PROBLEM: Developer workspace drowned in dependency docs and web-search tabs; soft red API-error glow. Cut briefly past a real spur context --help terminal (reference images). Tension: agents are weak on dependencies.

BEAT 2 (3-6s) SETUP + INDEX: Terminal becomes spur context key / mcp setup, then a floating package crate serde@1.0.197 opens into a luminous 3D code graph of nodes and edges. Version pin glows green.

BEAT 3 (6-9s) TOOLS: UI panels animate external_knowledge_context evidence pack then external_code_read of pkg:serde@1.0.197::Deserialize. Match the reference terminal: selectors, confidence high, pinned source. Agent selects green symbol node.

BEAT 4 (9-12s) TWO PLANES + CTA: Split — left Local worktree code_*, right External external_*. Cyan bridge. Title card: SPUR Context Service. Subtitle: Version-precise context for third-party packages. Cinematic, tack sharp, premium B2B SaaS commercial.'

echo "==> Seedance 2.0 (12s, 1080p) with terminal refs…"
higgsfield generate create seedance_2_0 \
  --prompt "$PROMPT" \
  --start-image "$FRAMES/01-setup-mid.png" \
  --image "$FRAMES/02-knowledge-mid.png" \
  --image "$FRAMES/02-read-late.png" \
  --image "$FRAMES/02-cta-end.png" \
  --end-image "$FRAMES/02-cta-end.png" \
  --duration 12 \
  --aspect_ratio 16:9 \
  --resolution 1080p \
  --mode std \
  --generate_audio true \
  --genre epic \
  --wait \
  --wait-timeout 20m \
  --json | tee "$OUT_JSON"

url="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[0].get("result_url") or "")' "$OUT_JSON" 2>/dev/null || true)"
if [[ -n "$url" ]]; then
  echo
  echo "Marketing video URL:"
  echo "$url"
  printf '%s\n' "$url" >"$ROOT/out/marketing-video.url"
fi
