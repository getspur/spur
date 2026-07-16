# SPUR Product Hunt — Media Pack (real VHS captures only)

**Date:** 2026-07-16  
**Rule:** Product Hunt media is **real SPUR TUI film** from `scripts/e2e/demos/tui-live/`. No AI-invented terminal UIs.  
**Sources:**

| Source | Role |
|---|---|
| `scripts/e2e/demos/tui-live/out/*.mp4|gif` | Rendered VHS captures (upload truth) |
| `scripts/e2e/demos/tui-live/tapes/*.tape` | Storyboard scripts that produced the films |
| `docs/product_launch/product-journey-ph.md` | Journey order / Session Detail home |
| `SPUR_PRD.md` v2.3 | Claims, tiers, positioning |

**Re-film command (when stills go stale):**

```bash
cd scripts/e2e/demos/tui-live
export SPUR_BIN="$(command -v spur)"
SPUR_DEMO_STORIES_ONLY=1 SPUR_DEMO_STORY_PACE=1 ./render.sh
# then re-run scripts/product_launch/refresh-media-pack.sh (or this pack’s extract recipe below)
```

---

## Positioning lock

| Element | Value |
|---|---|
| Tagline | Control tower for CLI coding agents. |
| Operator home | **Session Detail** (not Dashboard-first) |
| Hero journey | `problem-plan-loop-drive` → `out/13-problem-plan-loop-drive.{mp4,gif}` |
| Secondary | `product-e2e-flow` → `09-…` |
| Durability cut | `session-resume` → `04-…` |

---

## Layout

```text
docs/product_launch/media_pack/
├── MANIFEST.md
├── html/index.html              # local visualizer (real paths only)
├── live_demos/                  # copies of VHS out/ mp4+gif
├── gallery_stills/              # mid-film PNG frames (ffmpeg)
├── ph_ready/                    # PH-sized derivatives + hero copies
└── tapes_index/                 # source .tape storyboards
```

---

## A. Live demos (PH video / GIF truth)

| Pack path | Tape | Journey problem |
|---|---|---|
| `live_demos/13-problem-plan-loop-drive.{mp4,gif}` | `tapes/13-problem-plan-loop-drive.tape` | submit_plan loop black box → drive brain↔worker |
| `live_demos/09-product-e2e-flow.{mp4,gif}` | `tapes/09-product-e2e-flow.tape` | specialist + model/effort without losing context |
| `live_demos/10-problem-ops-visibility.{mp4,gif}` | `tapes/10-…` | what’s running where I work |
| `live_demos/11-problem-plan-progress.{mp4,gif}` | `tapes/11-…` | multi-task campaign progress |
| `live_demos/12-problem-backlog-triage.{mp4,gif}` | `tapes/12-…` | P0 backlog triage |
| `live_demos/04-session-resume.{mp4,gif}` | `tapes/04-…` | re-attach / resume |
| `live_demos/14-live-plan-loop-seed.{mp4,gif}` | live seed capture | optional real spend seed |

**PH upload:**

| Field | File |
|---|---|
| **Video (YouTube)** | `ph_ready/hero-video-plan-loop-drive.mp4` (= `13-…mp4`) |
| **Hero GIF** | `ph_ready/hero-gif-plan-loop-drive.gif` |
| Optional secondary GIF | `live_demos/09-product-e2e-flow.gif` |

---

## B. Gallery stills (ffmpeg from real mp4)

All under `gallery_stills/` and PH-sized under `ph_ready/gallery-0N-*-1270x760.png`.

| # | Still | Source film | Caption |
|---:|---|---|---|
| 1 | `01-session-plan-loop` | 13 plan-loop | Session Detail home |
| 2 | `02-workers-delegate` | 13 plan-loop | Workers / DELEGATE in session |
| 3 | `03-plan-progress` | 11 plan-progress | Campaign Progress |
| 4 | `04-explore-cascade` | 09 product-e2e | Explore + specialist cascade |
| 5 | `05-session-resume` | 04 resume | Session resume |
| 6 | `06-backlog-triage` | 12 backlog | P0 backlog triage |
| 7 | `07-ops-visibility` | 10 ops | Ops visibility (optional #7) |

**Thumbnail:** crop of real hero frame → `ph_ready/thumbnail-240.png` / `thumbnail-512.png`  
(Real TUI crop — not a synthetic logo.)

---

## C. What is explicitly **not** in this pack

- AI-generated “marketing terminal” mockups that invent UI chrome  
- Higgsfield GPT Image gallery replacements of Session Detail  
- Cost-ledger hero stills not present in the problem-story films  

If you need social OG art later, generate it **separately** and never substitute it for gallery product screenshots.

---

## D. Extract recipe (reproducible)

```bash
OUT=scripts/e2e/demos/tui-live/out
PACK=docs/product_launch/media_pack
# densest frame among candidates:
ffmpeg -ss <t> -i $OUT/13-problem-plan-loop-drive.mp4 -frames:v 1 $PACK/gallery_stills/01-session-plan-loop.png
ffmpeg -i $PACK/gallery_stills/01-session-plan-loop.png \
  -vf "scale=1270:760:force_original_aspect_ratio=increase,crop=1270:760" \
  $PACK/ph_ready/gallery-01-01-session-plan-loop-1270x760.png
```

Open visualizer: `docs/product_launch/media_pack/html/index.html`
