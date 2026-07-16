# SPUR marketing media (Higgsfield, product-truth conditioned)

**Date:** 2026-07-16  
**Rule:** `gallery_stills/` + `live_demos/` / `ph_ready/` are **product truth** (real VHS).  
This folder is **marketing only** — generated with Higgsfield using those assets as `--image` / `--start-image` / `--end-image` references. Never upload marketing fakes as Product Hunt gallery product screenshots.

## Source of truth (inputs)

| Reference | Path | Used for |
|---|---|---|
| Hero frame | `../gallery_stills/00-hero-frame.png` | OG social, trailer start |
| Session Detail | `../gallery_stills/01-session-plan-loop.png` | Social square |
| Workers / DELEGATE | `../gallery_stills/02-workers-delegate.png` | Trailer mid ref |
| Plan Progress | `../gallery_stills/03-plan-progress.png` | Plan story card, trailer end |
| Explore cascade | `../gallery_stills/04-explore-cascade.png` | Explore story card |
| Session resume | `../gallery_stills/05-session-resume.png` | Resume story card |

Positioning (from `SPUR_PRD.md` + launch checklist):

- Tagline: **Control tower for CLI coding agents.**
- Brand: *One brain, many workers, zero lost context.*
- Home surface: Session Detail (not Dashboard-first)

## Outputs (`out/`)

| File | Model | Job | Channel use |
|---|---|---|---|
| `01-og-social.png` | GPT Image 2 | `ea5d11b4-…` | Website OG, X/LinkedIn link preview (16:9) |
| `02-social-square.png` | GPT Image 2 | `21715b0c-…` | X/LinkedIn square post |
| `03-plan-story.png` | GPT Image 2 | `be199884-…` | Thread slide: plan/worker loop |
| `04-explore-story.png` | GPT Image 2 | `08f02825-…` | Thread slide: specialist cascade |
| `05-resume-story.png` | GPT Image 2 | `a39de4bd-…` | Thread slide: session resume |
| `06-trailer.mp4` | Seedance 2.0 | `d10449e1-…` | Social trailer (8s); **not** PH product video |

## Product Hunt vs marketing

| Asset class | Source | PH upload? |
|---|---|---|
| Gallery screenshots | `../ph_ready/gallery-*.png` (real crop) | **Yes** |
| PH thumbnail | `../ph_ready/thumbnail-240.png` (real crop) | **Yes** |
| PH product video | `../ph_ready/hero-video-plan-loop-drive.mp4` (real VHS) | **Yes** |
| OG / social / trailer | `marketing/out/*` (Higgsfield + real refs) | **No** (social/site only) |

## Regenerate

```bash
# After refresh.sh updates stills:
PACK=docs/product_launch/media_pack
STILLS=$PACK/gallery_stills
MKT=$PACK/marketing/out

higgsfield generate create gpt_image_2 \
  --prompt "..." --image $STILLS/00-hero-frame.png \
  --aspect_ratio 16:9 --resolution 2k --wait

higgsfield generate create seedance_2_0 \
  --prompt "..." \
  --start-image $STILLS/00-hero-frame.png \
  --end-image $STILLS/03-plan-progress.png \
  --duration 8 --aspect_ratio 16:9 --wait
```

Job IDs and create payloads: `jobs/*.create.json` / `jobs/*.done.json`.
