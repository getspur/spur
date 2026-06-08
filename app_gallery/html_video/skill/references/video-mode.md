# html-video mode reference

This reference defines the content graph, per-frame output protocol, and the render
contract for `html-video`.

## Content-graph cell JSON format

The plan cell created in the `plan` step is the canonical pipeline source. It must be
valid JSON inside a markdown cell and should be versioned as `"kind": "html-video"`.

```json
{
  "version": "1",
  "kind": "html-video",
  "title": "Launch Reel",
  "duration_seconds": 12,
  "fps": 30,
  "size": { "width": 1080, "height": 1920 },
  "template_id": "template-id-from-manifest",
  "template_overrides": {
    "palette": {
      "bg": "#060f2f",
      "accent": "#ffd166",
      "text": "#ffffff"
    },
    "font": "Inter"
  },
  "frames": [
    {
      "id": "frame-01",
      "index": 0,
      "duration_ms": 3000,
      "copy": {
        "headline": "Hook line",
        "body": "Optional supporting text"
      },
      "motion": {
        "in": "fade-in",
        "out": "fade-out"
      }
    },
    {
      "id": "frame-02",
      "index": 1,
      "duration_ms": 3000,
      "copy": {
        "headline": "Feature callout"
      },
      "motion": {
        "in": "slide-left",
        "out": "slide-up"
      }
    }
  ]
}
```

- `duration_seconds` and `fps` define render timing expectations.
- `size` should match target output ratio.
- `frames` drives the number and order of per-frame HTML cells.
- Keep optional fields compact so frame cells can stay deterministic.

## Per-frame HTML cell protocol

Each element in `frames` maps to one notebook code cell with:

- `kind: "code"`
- `code_type: "html"` or your runtime equivalent Deno HTML code type
- one and only one output item with MIME `text/html`
- no external network or CDN assets
- frame payload embedded as JSON in-page (`window.__FRAME__`) or compile-time constants
- exactly one capture target: `<canvas data-capture="true">`

Minimal frame structure used by template renderers:

```html
<style>/* inlined frame CSS */</style>
<canvas data-capture="true" width="1080" height="1920"></canvas>
<script>
  const frame = window.__FRAME__ || {}
  const durationMs = frame.duration_ms || 3000
  const canvas = document.querySelector('canvas[data-capture="true"]')
  const ctx = canvas.getContext("2d")

  function draw(progress) {
    ctx.fillStyle = "#060f2f"
    ctx.fillRect(0, 0, canvas.width, canvas.height)
    ctx.fillStyle = "#ffffff"
    ctx.font = "700 96px Inter, sans-serif"
    ctx.fillText(frame.copy?.headline || "Hook line", 96, 240 + progress * 80)
  }

  draw(0)
</script>
```

The notebook runtime records the marked canvas in the browser and reports captures
to the notebook bridge with this message shape:

```json
{
  "type": "jute-video-capture",
  "cellId": "frame-cell-id",
  "webm": "base64-webm-payload",
  "duration_sec": 3
}
```

## Render recipe

After frame cells render, collect browser captures through the notebook MCP bridge:

```json
{
  "tool": "notebook_get_cell_capture",
  "arguments": { "cell_id": "frame-cell-id" }
}
```

The capture result includes `webm_base64` and `duration_sec`. Keep the captures in
the same order as `frames`, then call:

```json
{
  "tool": "html_video_render",
  "arguments": {
    "webm_frames": ["base64-webm-payload"],
    "output_path": "artifacts/output.mp4"
  }
}
```

Embed the rendered MP4 from `output_path` in the final notebook output cell.

## Video output cell format

Final notebook output must be a single `text/html` cell containing:

```html
<video controls preload="metadata" playsinline width="1080" height="1920" src="data:video/mp4;base64,..."></video>
```

Use one base64 payload per render pass and keep the cell self-contained, no external
`<source>` URLs, no local `file://` references.
