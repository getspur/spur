# PH hero demo render (html-video style)

Implements the **approved** video-review backlog for V1 product hero:

1. Trim cold open (start at **5s** of real VHS)
2. Keep proof segment **5–40s** (~35s product truth)
3. HTML title + end cards (html-video frames)
4. Burned-in caption strips (HTML → PNG overlay)
5. Export **1920×1080 16:9** for Product Hunt / YouTube

## Source of truth

| Input | Path |
|---|---|
| Real VHS film | `../ph_ready/hero-video-plan-loop-drive.mp4` (= `tui-live/out/13-problem-plan-loop-drive.mp4`) |
| IR / plan | `content-graph.json` |
| HTML frames | `html/01-title.html`, `html/03-end.html`, caption strips |
| Output | `out/spur-ph-hero-demo.mp4` → published as `../ph_ready/hero-video-ph-ready.mp4` |

## Rebuild

```bash
cd docs/product_launch/media_pack/demo_render
./build.sh
```

Requires: `ffmpeg`, Node 18+, Google Chrome (for puppeteer-core screenshots).

## Timeline (41s)

| t | Segment |
|---|---|
| 0–3s | Title card — *Control tower for CLI coding agents.* |
| 3–38s | Real plan-loop demo (VHS trim 5→40) + captions |
| 38–41s | End card — install one-liner |

Captions (relative to demo segment):

- 0–5s Session Detail — operator home  
- 8–14s Drive brain ↔ worker loops  
- 16–22s Plans and progress in one surface  
- 24–30s Specialists without losing context  
- 30–35s Resume after you close the laptop  

## Product Hunt upload

Use **`ph_ready/hero-video-ph-ready.mp4`** (this render), not the raw untrimmed VHS and not the Seedance social trailer.
