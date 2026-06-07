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

Minimal frame structure used by template renderers:

```html
<style>/* inlined frame CSS */</style>
<div id="frame" data-frame-index="0" data-frame-ms="3000">
  <h1>{{headline}}</h1>
  <p>{{body}}</p>
</div>
<script>
  const frame = window.__FRAME__ || {}
  const durationMs = frame.duration_ms || 3000
  // deterministic animation script only
  setTimeout(() => {
    document.body.dataset.done = "true"
  }, durationMs)
</script>
```

## Render cell recipe

Use a single Deno-render cell to materialize all frames into MP4. The command path is
either:

- `node <html-video-cli>` (preferred when available), passing the content-graph and
  all generated frame HTML outputs.
- raw Playwright + ffmpeg in Deno subprocess mode when CLI is not available.

Example render sequence:

- Write rendered frame HTML files to temporary disk.
- Render each frame to an image sequence or direct clip via Playwright screenshots.
- Invoke ffmpeg to encode and concat all frame clips into one `output.mp4`.
- Base64-encode `output.mp4` for notebook embedding.
- Emit a final `text/html` cell containing the inline video player.

Example command skeleton:

```js
// deno run -A
const graph = JSON.parse(await Deno.readTextFile("content-graph.json"))
const nodeCmd = ["node", "<html-video-cli>", "--graph", "content-graph.json", "--out", "artifacts"]
const cli = new Deno.Command(nodeCmd[0], { args: nodeCmd.slice(1) }).spawn()
await cli.status
const ffmpegCmd = ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", "artifacts/files.txt", "-c:v", "libx264", "-pix_fmt", "yuv420p", "artifacts/output.mp4"]
await new Deno.Command(ffmpegCmd[0], { args: ffmpegCmd.slice(1) }).spawn().status
```

## Video output cell format

Final notebook output must be a single `text/html` cell containing:

```html
<video controls preload="metadata" playsinline width="1080" height="1920" src="data:video/mp4;base64,..."></video>
```

Use one base64 payload per render pass and keep the cell self-contained, no external
`<source>` URLs, no local `file://` references.
