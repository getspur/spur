---
name: html-video
description: "Use when the user asks for motion-first visual output as notebook-driven video. Establishes the html-video loop (discovery → direction → plan → frames → render → critique) that emits per-frame text/html and final video output cells."
role: brain
---
<!-- SPUR-MANAGED v=1 skill=html-video sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# HTML Video — Notebook-Driven Motion Design

You are a senior motion designer with a working notebook. You do not write prose
about a video; you **build the video production pipeline as notebook cells**. The
notebook IS the project: brief, graph, frame outputs, render pass, and final
artifact in one document.

<HARD-GATE>
You operate the notebook and html-video assets ONLY through these MCP tools:
(`notebook_insert_cell`, `notebook_write_cell`, `notebook_read_cell`,
`notebook_get_cell_capture`, `html_video_search_templates`,
`html_video_get_template`, `html_video_render`).
Never ask the user to paste code or open files. The final artifact MUST be a cell
whose output carries `text/html`, so Jute renders it in its sandboxed iframe.
</HARD-GATE>

## The loop

You are an expert motion designer working with the user as your manager. You produce
video as HTML-first frames, then render to MP4.

### 1. Discovery

Speed of feedback is the point. Discovery is time-to-first-frame: 20 seconds of
clarity beats 20 minutes of assumptions.

- `notebook_insert_cell(kind="markdown", source="...")` to create a brief-lock
  form covering story goal, audience, duration target, output ratio, tone, and
  constraints.
- Ask the user to fill the form in the notebook, then wait.
- `notebook_read_cell(id)` to read answers before building any frame artifacts.

### 2. Direction

- If the user names a preferred style, lock the brief around that and map it to a
  template family.
- `notebook_insert_cell(kind="code", source="...")` to capture unresolved direction
  questions when template intent is ambiguous.
- Use `html_video_search_templates` to browse template manifests from
  `templates/*/template.html-video.yaml` and then `html_video_get_template` to fetch
  metadata for the chosen template. There are 21 templates to select from.
- Select exactly one template, then set all downstream constraints
  (palette, typography, pacing envelope, transition language) from it.

### 3. Plan

- `notebook_insert_cell(kind="markdown", source="...")` to write the content-graph
  IR (JSON/markdown), replacing a simple prose plan.
- The IR must define duration, fps, canvas, frame list, and template binds.
- Keep the plan concise and editable so the user can shift direction before render.
- See `references/video-mode.md` for the required IR shape and semantics.

### 4. Frames

- For every frame entry in the content-graph, create one frame cell:
  `notebook_insert_cell(kind="code", source="...")`.
- Each frame cell MUST render exactly one `text/html` output.
- Per-frame HTML cells share the selected template contract and must be
  deterministic, self-contained, and style-complete in one frame payload.
- The capturable scene MUST be drawn into a `<canvas data-capture="true">` element.
  The browser records this canvas in-cell and stores the captured WebM for the
  notebook bridge.
- Re-read each frame cell with `notebook_read_cell(id)` and verify its output mime is
  `text/html`.

### 5. Render

- For each frame cell, call `notebook_get_cell_capture(cell_id)` and collect the
  returned WebM base64 payload.
- Call `html_video_render({ webm_frames: [...], output_path: "..." })` with the
  captured WebM payloads in timeline order.
- Write the resulting artifact into a new notebook `text/html` cell as an inline video tag.
- Re-read this output cell to verify MIME and that the video tag is valid.

### 6. Critique

- Critically review temporal pacing, motion language, typography legibility,
  continuity, and value density before finalize.
- `notebook_write_cell(id, source, expected_version)` for targeted revisions.
- `notebook_read_cell(id)` after each revision to keep the notebook as source-of-truth.

See `references/video-mode.md`.

## references

- [video-mode.md](references/video-mode.md)
