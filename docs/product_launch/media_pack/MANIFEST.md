# SPUR Product Hunt media pack

**Reviewed:** 2026-07-16
**Capture mode:** observe-only
**Rule:** Product Hunt product proof comes from fresh SPUR TUI captures in `scripts/e2e/demos/tui-live/`. Generated marketing material is not product proof.

## What to upload

| Product Hunt slot | Approved file | Specification | Visible proof |
|---|---|---|---|
| Thumbnail | `ph_ready/thumbnail-240.png` | 240 by 240 PNG | SPUR identity |
| Gallery 1 | `ph_ready/gallery-01-session-detail-1270x760.png` | 1270 by 760 PNG | Session Detail, INSERT, following |
| Gallery 2 | `ph_ready/gallery-02-worker-visibility-1270x760.png` | 1270 by 760 PNG | WORKERS, running and cancelled state |
| Gallery 3 | `ph_ready/gallery-03-plan-state-1270x760.png` | 1270 by 760 PNG | Plans, No plans found |
| Gallery 4 | `ph_ready/gallery-04-specialist-routing-1270x760.png` | 1270 by 760 PNG | agent, model, effort |
| Gallery 5 | `ph_ready/gallery-05-session-resume-1270x760.png` | 1270 by 760 PNG | Session, Resumed from prior conversation |
| Video | `ph_ready/hero-video-ph-ready.mp4` | 25.2 seconds, H.264, 1920 by 1080 | Five reviewed source segments |

Open `html/index.html` for the scripts-off launch handoff. It includes the upload inventory, captions, product-proof frames, and provenance links without remote resources.

## Evidence map

`proof-manifest.json` is the canonical approval record. It binds each asset to a source file, journey, timestamp, crop, approved SHA-256, visible proof terms, caption, output name, and channel.

| ID | Source | Time | Approved claim |
|---|---|---:|---|
| `session-detail-home` | `live_demos/13-problem-plan-loop-drive.mp4` | 10.0s | Session Detail keeps the operator in one working context. |
| `worker-visibility` | `live_demos/10-problem-ops-visibility.mp4` | 20.0s | Go to exposes running and cancelled worker state. |
| `plan-state` | `live_demos/11-problem-plan-progress.mp4` | 14.0s | The plan hub makes an empty execution slot explicit. |
| `specialist-routing` | `live_demos/09-product-e2e-flow.mp4` | 54.0s | Agent, model, and effort remain explicit before dispatch. |
| `session-resume` | `live_demos/04-session-resume.mp4` | 8.0s | A saved conversation returns to the same operator surface. |

The observe-only capture has no seeded campaign and no active EXEC worker. Gallery 3 therefore proves an explicit empty plan state, not campaign progress. Gallery 2 proves worker-state visibility through the Go to surface, not a live brain-to-worker loop. Do not strengthen either claim without a new approved capture.

## Rebuild and verify

Prerequisites: `ffmpeg`, `ffprobe`, `jq`, Node 18 or later, Google Chrome, `tesseract`, and VHS for recapture.

```bash
# Optional: recapture the reviewed journeys without sending agents.
cd scripts/e2e/demos/tui-live
export SPUR_BIN="$(command -v spur)"
SPUR_DEMO_STORIES_ONLY=1 SPUR_DEMO_STORY_PACE=1 ./render.sh

# Publish checksum-bound stills and thumbnail.
cd ../../../../
docs/product_launch/media_pack/refresh.sh

# Build the multi-source hero.
docs/product_launch/media_pack/demo_render/build.sh

# Verify journey and media contracts.
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

`refresh.sh` fails closed before publishing. It checks source existence, approved checksum, timestamp, crop bounds, output dimensions, and OCR proof terms in a staging directory, then swaps the validated outputs into place.

The live-seed path is intentionally outside the default capture. It can send agents and spend credits. Run it only with explicit approval and the repository's spend gate enabled.

## Repository layout

```text
docs/product_launch/media_pack/
├── proof-manifest.json          # canonical evidence and approval map
├── refresh.sh                  # fail-closed derivative publisher
├── html/index.html             # static launch handoff
├── live_demos/                 # reviewed source MP4 files
├── gallery_stills/             # timestamp and crop extracts
├── ph_ready/                   # approved upload derivatives
├── demo_render/                # deterministic hero renderer
├── tapes_index/                # reviewed journey tapes
├── marketing/                  # separate generated marketing layer
└── tests/media-contract.test.sh
```

## Channel boundary

Use `ph_ready/` assets for Product Hunt product proof. Content under `marketing/out/` may be used for social posts, ads, or brand motion only. Generated motion can invent intermediate UI and must never replace the real gallery images or the product demo.
