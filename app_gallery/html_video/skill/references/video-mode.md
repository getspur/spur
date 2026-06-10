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

## Template discovery

Templates are discovered from `templates/index.json`, which lists the 5 available
templates. Use `html_video_search_templates` to browse them and
`html_video_get_template` to fetch a template's metadata and HTML. There are
**5 templates** — not 21.

## Per-frame HTML cell protocol

Each element in `frames` maps to one notebook code cell with:

- `kind: "code"`
- `code_type: "html"` or your runtime equivalent Deno HTML code type
- one and only one output item with MIME `text/html`
- no external network or CDN assets
- frame payload embedded as JSON in-page (`window.__FRAME__`) or compile-time constants
- exactly one capture target: `<canvas data-capture="true" data-capture-duration-sec="...">`

The `data-capture-duration-sec` attribute tells the recorder how long to record.
It corresponds to the `duration_sec` stored in the port manifest entry and used by
the render server as the frame's duration.

Minimal frame structure used by template renderers:

```html
<style>/* inlined frame CSS */</style>
<canvas data-capture="true" data-capture-duration-sec="60" width="1080" height="1920"></canvas>
<script>
  const frame = window.__FRAME__ || {}
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

When active output scripts are enabled (granted via the one-time trust prompt), the
browser records this canvas and delivers the WebM to the notebook bridge as a
`jute-video-capture` postMessage. The `push_capture_port` command stores the WebM
in the port store under the declared port name, along with the `duration_sec` value.

## Render recipe

After frame cells render and the captures have been written to the port store,
call `html_video_render` with `port_names` to read directly from the store:

```json
{
  "tool": "html_video_render",
  "arguments": {
    "port_names": ["spur-ad-capture"],
    "output_path": "artifacts/output.mp4",
    "fps": 30
  }
}
```

The server reads each port's bytes from `entry["path"]` in the manifest (never
from a bare `root/<port-name>` path). When `frame_duration` is not passed, the
server uses the `duration_sec` stored in the manifest entry (set by the recorder
from `data-capture-duration-sec`), falling back to 3.0 seconds as a last resort.

Embed the rendered MP4 from `output_path` in the final notebook output cell.

## Video output cell format

Final notebook output must be a single `text/html` cell containing:

```html
<video controls preload="metadata" playsinline width="1080" height="1920" src="data:video/mp4;base64,..."></video>
```

Use one base64 payload per render pass and keep the cell self-contained, no external
`<source>` URLs, no local `file://` references.
