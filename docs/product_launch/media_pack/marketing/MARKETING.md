# SPUR marketing media (Higgsfield, product-truth conditioned)

**Date:** 2026-07-16 (photoshoot pack approved 2026-07-20)  
**Rule:** `gallery_stills/` + `live_demos/` / `ph_ready/` are **product truth** (real VHS).  
This folder is **marketing only** — generated with Higgsfield using those assets as `--image` / `--start-image` / `--end-image` references. Never upload marketing fakes as Product Hunt gallery product screenshots.

## Source of truth (inputs)

| Reference | Path | Used for |
|---|---|---|
| Hero frame | `../gallery_stills/00-hero-frame.png` | OG social, trailer start |
| Session Detail | `../gallery_stills/session-detail-home.png` | Social square, product photoshoot 07 |
| Workers / DELEGATE | `../gallery_stills/worker-visibility.png` | Trailer mid ref, product photoshoot 08 |
| Plan Progress | `../gallery_stills/plan-state.png` | Plan story card, trailer end |
| Specialist routing | `../gallery_stills/specialist-routing.png` | Product photoshoot 09 |
| Session resume | `../gallery_stills/session-resume.png` | Resume story card, product photoshoot 10 |
| Brand mark | `../ph_ready/thumbnail-512.png` | Product photoshoot identity lock |

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
| `07-product-session-detail.png` | product-photoshoot / GPT Image 2 | `c2befea9-…` | Product photoshoot: Session Detail hero (2048²) |
| `08-product-worker-visibility.png` | product-photoshoot / GPT Image 2 | `65596171-…` | Product photoshoot: workers control tower |
| `09-product-specialist-routing.png` | product-photoshoot / GPT Image 2 | `9713df68-…` | Product photoshoot: specialist routing |
| `10-product-session-resume.png` | product-photoshoot / GPT Image 2 | `a72a9a00-…` | Product photoshoot: session resume |

### Product photoshoot pack (approved 2026-07-20)

Mode: `product_shot` via `higgsfield product-photoshoot create`.  
Refs: live `gallery_stills/*` + `ph_ready/thumbnail-512.png`.  
Job logs: `out/photoshoot-2026-07-20/` and `jobs/<job_id>.done.json`.

| Asset | Proof surface | SHA-256 (first 16) |
|---|---|---|
| `07-product-session-detail.png` | Session Detail home | `41ef0d7d50fb6cc1…` |
| `08-product-worker-visibility.png` | Worker visibility | `65dc2d82b4f45cee…` |
| `09-product-specialist-routing.png` | Specialist routing | `e4a7b8be269e6b08…` |
| `10-product-session-resume.png` | Session resume | `b82b0ee3e4f802ef…` |

## Product Hunt vs marketing

| Asset class | Source | PH upload? |
|---|---|---|
| Gallery screenshots | `../ph_ready/gallery-*.png` (real crop) | **Yes** |
| PH thumbnail | `../ph_ready/thumbnail-240.png` (real crop) | **Yes** |
| PH product video | `../ph_ready/hero-video-plan-loop-drive.mp4` (real VHS) | **Yes** |
| OG / social / trailer / product photoshoot | `marketing/out/*` (Higgsfield + real refs) | **No** (social/site only) |

## Regenerate

```bash
# After refresh.sh updates stills:
PACK=docs/product_launch/media_pack
STILLS=$PACK/gallery_stills
MKT=$PACK/marketing/out
PH=$PACK/ph_ready

higgsfield generate create gpt_image_2 \
  --prompt "..." --image $STILLS/session-detail-home.png \
  --aspect_ratio 16:9 --resolution 2k --wait

higgsfield generate create seedance_2_0 \
  --prompt "..." \
  --start-image $STILLS/session-detail-home.png \
  --end-image $STILLS/plan-state.png \
  --duration 8 --aspect_ratio 16:9 --wait

# Product photoshoot pack (one job per surface; batch --count>1 enhance is flaky)
higgsfield product-photoshoot create \
  --mode product_shot \
  --count 1 \
  --prompt "clean studio hero product shot of SPUR Session Detail UI, dark premium tech" \
  --product_context "SPUR: control tower for CLI coding agents." \
  --brand_context "Dark UI, monochrome terminal aesthetic. Premium, operator-grade." \
  --image $STILLS/session-detail-home.png \
  --image $PH/thumbnail-512.png \
  --timeout 12m
```

Job IDs and create payloads: `jobs/*.create.json` / `jobs/*.done.json`.
