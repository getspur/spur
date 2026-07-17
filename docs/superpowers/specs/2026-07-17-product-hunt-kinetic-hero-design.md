# Product Hunt kinetic hero design

**Status:** approved direction, awaiting implementation plan  
**Decision:** A — Kinetic operator cut  
**Editor:** Palmier Pro  
**Visual review surface:** `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`

## Goal

Turn the verified 23.2-second Palmier baseline into a tighter 20-second Product Hunt hero. The enhancement must make the real SPUR TUI evidence easier to parse without inventing interface states or implying that continuous human review has been proven.

## Truth boundary

- Use only the five checksum-reviewed TUI recordings already imported into the Palmier project.
- Authored framing, labels, progress indicators, color, and camera movement may clarify the recordings but may not change their product meaning.
- Do not claim Reject → Retry → Approve or operator-owned review. The D4 attempt remains diagnostic and non-promotable.
- Do not invoke paid generation, synthesize UI, or replace the source recordings.
- Preserve the existing Palmier timeline and export as a baseline. Build the enhancement on a new timeline.

## Timeline

The enhanced timeline is 600 frames at 30 fps.

| Beat | Frames | Duration | Source span | Authored cue |
|---|---:|---:|---|---|
| Title | 0–42 | 1.4 s | SPUR ink matte | `SPUR` / `Control tower for CLI coding agents.` |
| Session | 42–144 | 3.4 s | Session Detail, 8.0–11.4 s | `01 / SESSION HOME` |
| Workers | 144–234 | 3.0 s | Worker visibility, 18.5–21.5 s | `02 / WORKERS VISIBLE` |
| Plan | 234–330 | 3.2 s | Plan state, 12.5–15.7 s | `03 / PLAN STATE` |
| Routing | 330–444 | 3.8 s | Specialist routing, 52.5–56.3 s | `04 / ROUTE SPECIALIST` |
| Resume | 444–540 | 3.2 s | Session resume, 5.8–9.0 s | `05 / RESUME CONTEXT` |
| CTA | 540–600 | 2.0 s | SPUR ink matte | `SPUR` / `Install Community free.` |

## Visual system

- Keep the established SPUR palette: ink `#0B0E14`, ivory `#E6E1CF`, signal cyan `#7FB4CA`, routing violet `#957FB8`, and border `#2A2E38`.
- Replace large centered lower thirds with compact upper-left numbered cues so the terminal status bar remains visible.
- Add a thin cyan progress rail near the lower safe area. Each beat advances one segment; it is authored launch framing, not product UI.
- Use direct cuts. Avoid ornamental transitions, glow effects, fake cursor motion, or synthetic interface animation.
- Apply one restrained push per proof beat: start at 100% and end between 104% and 108%, anchored toward the real evidence region. The movement must not crop command context or critical status text.
- Apply one shared, subtle color treatment across the five recordings. Preserve terminal legibility and avoid crushing dark UI surfaces.

## Palmier construction

1. Create a new `Kinetic Operator Cut` timeline at 1920×1080, 30 fps.
2. Reuse the six inspected media assets already in the project: five reviewed recordings plus the ink matte.
3. Place the exact source spans in the table on a single base-video track.
4. Add authored title, numbered cues, and CTA on text tracks.
5. Import locally authored transparent progress-rail overlays if Palmier has no native shape primitive.
6. Add per-clip transform keyframes and the shared color treatment.
7. Inspect title, every proof beat, and CTA in Palmier before export.
8. Export H.264 at 1080p to `ph_ready/hero-video-palmier-pro-enhanced.mp4` without replacing the baseline.

## Acceptance criteria

- Duration is exactly 20.0 seconds, 600 frames at 30 fps.
- Output is H.264, 1920×1080, `yuv420p`, and fully decodes with FFmpeg.
- All five reviewed proof beats remain present and in the approved order.
- Every cue is readable at 512-pixel inspection width and does not cover key TUI evidence.
- Camera motion is visible but restrained; no proof frame is cropped into ambiguity.
- The progress rail clearly advances across the five proof beats.
- No paid generation, generated UI, unreviewed recording, or stronger D4 claim appears.
- The baseline video and timeline remain intact.

## Verification

- Inspect representative frames through Palmier at the title, midpoint of each proof beat, and CTA.
- Poll Palmier export status until it reports completion.
- Verify codec, resolution, pixel format, frame rate, duration, and byte size with `ffprobe`.
- Decode the entire file with FFmpeg to a null sink.
- Build and inspect a seven-frame contact sheet.
- Record the final SHA-256 in the handoff; treat the enhanced file as a review candidate until explicitly promoted.

## Non-goals

- Voice-over, generated music, or sound effects.
- Product Hunt upload or YouTube publishing.
- Replacement of the approved proof manifest.
- Claims about continuous human review or the blocked D4 flow.
