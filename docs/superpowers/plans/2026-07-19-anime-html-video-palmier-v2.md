# Anime.js HTML Video Palmier V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce new 45-second and 90-second SPUR Product Hunt hero videos that combine three readable Anime.js motion plates with the existing real four-agent SPUR TUI recording, while preserving every existing Palmier timeline and export.

**Architecture:** The open `html_video` Jute notebook remains the motion source of truth. Three JavaScript cells emit self-contained `text/html` canvas captures; each output inlines the pinned Anime.js 4.4.1 UMD bundle and records to a distinct notebook media port. The notebook render cell converts those ports into exact 1920×1080/30fps MP4 plates, and Palmier Pro duplicates the existing diagnostic timelines before replacing their old title/CTA structure with the new 5s opener, 4s workflow rail, continuous real TUI segment, and 5s end card.

**Tech Stack:** Anime.js v4.4.1, Jute Notebook MCP, `html_video_render`, Palmier Pro MCP, ffmpeg/ffprobe, shell contract tests.

---

### Task 1: Strengthen the media copy contract

**Files:**
- Modify: `docs/product_launch/media_pack/tests/media-contract.test.sh`
- Inspect: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`

- [ ] **Step 1: Add a failing cross-project-leak assertion**

Add a repository-scoped assertion that fails when `beta.otobank.com` appears anywhere under `docs/product_launch/media_pack/`, and assert that the locked notebook brief contains `INSTALL SPUR · COMMUNITY FREE`.

- [ ] **Step 2: Run the contract test and confirm the intended failure**

Run:

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: FAIL if any stale Otobank copy remains; otherwise PASS proves the existing brief already satisfies the new assertion.

- [ ] **Step 3: Remove only stale SPUR-media Otobank copy**

Use Notebook MCP for notebook cell changes. Use `apply_patch` only for ordinary text files. Do not substitute another unconfirmed domain.

- [ ] **Step 4: Run the media contract again**

Run:

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: all media contracts pass.

- [ ] **Step 5: Commit the contract**

```bash
git add docs/product_launch/media_pack/tests/media-contract.test.sh docs/product_launch/media_pack/product-hunt-media-pack.ipynb
git commit -m "test(product-launch): D4.bb reject cross-project media copy"
```

### Task 2: Build three Anime.js notebook capture cells

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Output ports: `spur-ph-opener-v2`, `spur-ph-workflow-v2`, `spur-ph-end-card-v2`

- [ ] **Step 1: Replace the broken legacy capture cell with the 5-second opener**

Use `notebook_write_cell` on `spur-ad-capture`, then set its DAG source port to `spur-ph-opener-v2`. The cell must fetch the pinned `animejs@4.4.1/dist/bundles/anime.umd.min.js` source once in the Deno kernel, inline it into the returned HTML, and emit one 1920×1080 canvas with `data-capture-duration-sec="5"` and `data-capture-fps="30"`.

The visible copy is:

```text
SPUR
One durable outer harness for any coding agent that supports ACP.
REAL PROJECT · REAL AGENTS · ONE CONTROL LOOP
```

The canvas animation uses Anime.js only to interpolate deterministic numeric state; drawing remains a pure function of that state. Motion is limited to opacity and translate/scale reveals, with a scripts-off first frame and reduced-motion-safe legibility.

- [ ] **Step 2: Insert the 4-second workflow rail cell**

Use `notebook_insert_cell` after `spur-ad-capture`, then set the new cell metadata source to `{"kind":"canvas-capture","port":"spur-ph-workflow-v2"}`. Visible copy:

```text
USER ↔ BRAIN AGENT ↔ WORKER AGENTS
submit_plan · delegate · review · resume
CLAUDE CODE · GROK · CODEX · OPENCODE
```

- [ ] **Step 3: Insert the 5-second end-card cell**

Insert after the workflow cell and set its source port to `spur-ph-end-card-v2`. Visible copy:

```text
INSTALL SPUR
COMMUNITY FREE
Bring any ACP coding agent. Keep the control loop.
```

No domain may appear.

- [ ] **Step 4: Run and inspect each capture cell**

Call `notebook_run_cell` for all three cells, allow the 5s/4s/5s captures to finish, then call `notebook_read_cell` for each. Expected: `status=idle`, exactly one `text/html` output, no syntax/runtime error, and three media ports in the notebook DAG status.

- [ ] **Step 5: Run the static app doctor**

Call:

```text
notebook_app_doctor(path="/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb", level="static")
```

Expected: no structural or capability failures.

### Task 3: Render and verify the three motion plates

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Create: `docs/product_launch/media_pack/ph_ready/motion/anime-opener-v2-5s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/motion/anime-workflow-v2-4s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/motion/anime-end-card-v2-5s.mp4`

- [ ] **Step 1: Rewrite the notebook render cell**

Use `notebook_write_cell` on `spur-ad-render`. It must call `html_video_render` three times with one port per request, absolute worktree output paths, `resolution: "1920x1080"`, and `fps: 30`. Do not pass a conflicting `frame_duration`; the capture manifest duration is authoritative.

- [ ] **Step 2: Run the render cell**

Call `notebook_run_cell(cell_id="spur-ad-render")`.

Expected: three successful results reporting durations 5, 4, and 5 seconds.

- [ ] **Step 3: Rewrite the final notebook artifact cell**

Use `notebook_write_cell` on `spur-ad-video-embed` to emit a `text/html` contact sheet containing three labeled `<video controls>` previews, exact duration/copy metadata, and an explicit `No external domain` status. The HTML output is the notebook deliverable.

- [ ] **Step 4: Verify the plate files**

Run:

```bash
ffprobe -v error -show_entries stream=codec_name,width,height,r_frame_rate -show_entries format=duration -of json docs/product_launch/media_pack/ph_ready/motion/anime-opener-v2-5s.mp4
ffprobe -v error -show_entries stream=codec_name,width,height,r_frame_rate -show_entries format=duration -of json docs/product_launch/media_pack/ph_ready/motion/anime-workflow-v2-4s.mp4
ffprobe -v error -show_entries stream=codec_name,width,height,r_frame_rate -show_entries format=duration -of json docs/product_launch/media_pack/ph_ready/motion/anime-end-card-v2-5s.mp4
ffmpeg -v error -i docs/product_launch/media_pack/ph_ready/motion/anime-opener-v2-5s.mp4 -f null -
ffmpeg -v error -i docs/product_launch/media_pack/ph_ready/motion/anime-workflow-v2-4s.mp4 -f null -
ffmpeg -v error -i docs/product_launch/media_pack/ph_ready/motion/anime-end-card-v2-5s.mp4 -f null -
```

Expected: H.264, 1920×1080, 30fps, exact 5.000/4.000/5.000 durations, and no decode errors.

- [ ] **Step 5: Commit the rendered plates**

```bash
git add docs/product_launch/media_pack/ph_ready/motion
git commit -m "feat(product-launch): D4.bc render Anime.js motion plates"
```

### Task 4: Assemble new non-destructive Palmier V2 timelines

**Assets:**
- Existing source TUI mediaRef: `F2C142AD`
- Existing music mediaRefs: `23DF6A98` (45s), `668F693F` (90s)
- Existing draft timeline IDs: `F04395E8` (45s), `5CA824B6` (90s)
- New plate files from Task 3

- [ ] **Step 1: Re-open and inspect the Palmier project**

Use `manage_project(action="open", name="SPUR Product Hunt Hero - Real TUI")`, `get_media`, and `get_timeline`. Confirm the existing media refs and source durations before any edit.

- [ ] **Step 2: Import the three versioned motion plates**

Use `import_media` with the absolute paths from Task 3, then poll the returned media refs with `get_media(ids=[...])` until each reports ready. Inspect each imported plate before placing it.

- [ ] **Step 3: Duplicate the 45-second draft timeline**

Create a new timeline from `F04395E8`, name it `V2 Anime + Real TUI — 45s`, activate it, and re-read it. Preserve the original timeline unchanged.

- [ ] **Step 4: Build the 45-second sequence**

Replace the old title/CTA structure with these exact contiguous spans:

```text
0–5s   Anime opener
5–9s   Anime workflow rail
9–40s  continuous diagnostic TUI source (watermark retained)
40–45s Anime end card
```

Use Palmier-returned frame positions for placement; never calculate timeline frames manually. Retain the existing 45-second music bed and agent roster/watermark overlays.

- [ ] **Step 5: Duplicate and build the 90-second sequence**

Duplicate `5CA824B6` as `V2 Anime + Real TUI — 90s`, then build:

```text
0–5s   Anime opener
5–9s   Anime workflow rail
9–85s  continuous diagnostic TUI source (watermark retained)
85–90s Anime end card
```

Retain the existing 90-second music bed and agent roster/watermark overlays.

- [ ] **Step 6: Re-read both V2 timelines**

Expected: no gaps, no overlap beyond intentional overlay tracks, continuous TUI clips of 31s and 76s, and untouched draft timeline IDs `F04395E8` and `5CA824B6` still present.

### Task 5: Export, verify, and record the new drafts

**Files:**
- Create: `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-anime-v2-four-agent-45s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-anime-v2-four-agent-90s.mp4`
- Modify through Notebook MCP only: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`

- [ ] **Step 1: Queue versioned Palmier exports**

Export each active V2 timeline to its distinct absolute output path. Poll `manage_exports` until complete or a concrete error is reported.

- [ ] **Step 2: Verify exact final media properties**

Run `ffprobe` and full `ffmpeg -f null -` decodes on both files. Expected: H.264 1920×1080/30fps, AAC audio, exact 45.000 and 90.000 seconds, no decode errors.

- [ ] **Step 3: Sample visual checkpoints**

Extract frames around 2s, 7s, 20s, 42s for 45s and 2s, 7s, 45s, 87s for 90s. Confirm readable copy, real TUI continuity, diagnostic watermark, correct four-agent roster, and the domain-free end card.

- [ ] **Step 4: Update the media-pack notebook record**

Through Notebook MCP, append the V2 timeline IDs, exported filenames, exact duration checks, source TUI SHA256, and diagnostic/non-promotable status to the existing plan/record cell. Do not claim the source capture is promotable while its proof contract remains unresolved.

- [ ] **Step 5: Run final contracts and commit**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git add docs/product_launch/media_pack
git commit -m "feat(product-launch): D4.bd add Anime.js Palmier V2 hero cuts"
```

Expected: all media contracts pass and the final commit contains only SPUR media-pack artifacts and records.
