# Product Hunt hero renderer

Builds the approved 25.2-second SPUR product demo from five reviewed TUI captures. It does not treat a single source film as proof for unrelated claims.

## Inputs and output

| Input | Role |
|---|---|
| `../proof-manifest.json` | Approved source, timing, caption, and proof binding |
| `content-graph.json` | Ordered title, five video segments, and end card |
| `html/01-title.html` | Opening frame |
| `html/03-end.html` | Install frame |
| `../live_demos/*.mp4` | Fresh observe-only product captures |
| `../ph_ready/hero-video-ph-ready.mp4` | Published H.264, 1920 by 1080 output |

## Timeline

| Time | Segment | Source proof |
|---:|---|---|
| 0.0 to 3.0s | Title | Control tower for CLI coding agents. |
| 3.0 to 7.0s | Session | Session Detail, INSERT, following |
| 7.0 to 10.0s | Workers | WORKERS and worker state |
| 10.0 to 14.0s | Plans | Explicit empty plan state |
| 14.0 to 19.0s | Specialist | agent, model, effort |
| 19.0 to 22.2s | Resume | Resumed prior conversation |
| 22.2 to 25.2s | End | Install command |

The plan segment is intentionally captioned `Empty plan state is explicit.` The observe-only source does not contain a seeded campaign. The worker segment proves visible state, not a live delegation loop.

## Build

```bash
cd docs/product_launch/media_pack/demo_render
./build.sh
```

Requirements: `ffmpeg`, `ffprobe`, `jq`, Node 18 or later, and Google Chrome for `puppeteer-core` frame rendering.

The build validates the hero graph against the proof manifest, renders the title and caption frames, cuts each source segment by approved ID, normalizes every clip to 1920 by 1080 at 30 fps, concatenates them in graph order, and publishes the final MP4. `-nostdin` keeps sequential ffmpeg calls from consuming the build script's manifest stream.

## Verify

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

The contract checks graph-to-manifest bindings, caption-to-proof bindings, retained segment IDs, H.264 codec, 1920 by 1080 dimensions, 25.2-second duration, and the gallery evidence checks.

Use `../ph_ready/hero-video-ph-ready.mp4` for the Product Hunt video. Keep generated social trailers in `../marketing/out/` and off the product-proof gallery.
