# TypeScript SDK — @spur/app

Source: `sdk/typescript/`
Entry point: `sdk/typescript/mod.ts`
Runtime: Deno (primary); npm mirror for tooling compatibility.
Import: `import { callTool, display, capture, ports } from "@spur/app";`

## callTool

```ts
import { callTool, callToolWithSocket } from "@spur/app";
import type { CallToolOptions } from "@spur/app";
```

```ts
const result = await callTool(
  "html_video_render",
  { port_names: ["spur-ad-capture"], output_path: "spur-ad.mp4", fps: 30 },
  { clientInfo: { name: "spur-app", version: "0.1.0" } }   // optional
);
```

`callTool(name, args, options?) -> Promise<unknown>`

Reads `SPUR_NOTEBOOK_MCP_SOCKET` from the environment and delegates to
`callToolWithSocket`. Throws `Error("SPUR_NOTEBOOK_MCP_SOCKET is not set")` if
the env var is absent.

`callToolWithSocket(socketPath, name, args, options?) -> Promise<unknown>`

Like `callTool` but accepts an explicit socket path. Use this in tests with a
fake connection factory.

### Wire protocol

1. Connect to the Unix socket at `SPUR_NOTEBOOK_MCP_SOCKET`.
2. Send `initialize` request (id=1).
3. Read the `initialize` response; throw if it carries a JSON-RPC error.
4. Send `notifications/initialized` notification (no id).
5. Send `tools/call` request (id=2).
6. Read the `tools/call` response and unwrap structured content.

Frame layout: 4-byte big-endian `uint32` length prefix, followed by that many
UTF-8 JSON bytes. Implemented in `sdk/typescript/src/wire.ts` (`readFrame`,
`writeFrame`).

Structured-content unwrapping priority:
1. `result.structuredContent` (camelCase — MCP 2025-11-25)
2. `result.structured_content` (snake_case alias)
3. `result.content[].text` parsed as JSON, or `{ text }` if not JSON
4. `result` as-is

### CallToolOptions

```ts
interface CallToolOptions {
  clientInfo?: { name: string; version: string };
}
```

Default `clientInfo`: `{ name: "spur-app", version: "0.1.0" }`.

## capture

```ts
import { capture } from "@spur/app";
import type { CaptureCanvasOptions } from "@spur/app";
```

```ts
const html = capture.canvas({
  port: "my-cell-id",  // must match the DAG source cell id
  fps: 30,
  durationSec: 60,
  width: 1280,
  height: 720,
});
return display.html(html);  // last expression renders in Deno-Jupyter
```

`capture.canvas(opts: CaptureCanvasOptions) -> string`

Returns an HTML string for a `<canvas>` element with `data-capture` attributes
that trigger the host's video-capture recorder. The host appends a MediaRecorder
script to any HTML output containing `data-capture="true"`, records for
`durationSec` seconds, then posts `{ type: "jute-video-capture", cellId, webm,
duration_sec }` to `window.parent`. `OutputView.tsx` routes it to the Tauri
`push_capture_port` command using `event.data.cellId` as the port name.

**Important:** `port` must match the cell id that the DAG source declaration
uses. The host routes the postMessage by `cellId`; a mismatch means the capture
is stored under a different port name and `app.ports.read(...)` finds nothing.

### CaptureCanvasOptions

```ts
interface CaptureCanvasOptions {
  port: string;       // port name = cell ID; written to data-capture-cell-id
  fps?: number;       // default 30
  durationSec?: number;  // default 3
  width?: number;     // canvas width in CSS pixels; omitted if not provided
  height?: number;    // canvas height in CSS pixels; omitted if not provided
}
```

### Emitted attributes

```html
<canvas
  data-capture="true"
  data-capture-cell-id="<port>"
  data-capture-fps="<fps>"
  data-capture-duration-sec="<durationSec>"
  width="<width>"     <!-- only if width provided -->
  height="<height>"   <!-- only if height provided -->
></canvas>
```

## display

```ts
import { display } from "@spur/app";
import type { JupyterDisplay } from "@spur/app";
```

```ts
// Must be the last expression in the cell (or use return) — Deno-Jupyter only
// renders the cell's return value. A mid-cell statement is silently discarded.
return display.html("<b>Hello</b>")
return display.markdown("# Title")
return display.json({ key: "value" })
```

Each method returns a `JupyterDisplay` object with a `Symbol.for("Jupyter.display")`
method that returns the MIME bundle. **The object must be the last expression
or explicitly returned** — Deno-Jupyter renders only the cell return value.

| Method | MIME type |
|--------|-----------|
| `display.html(content: string)` | `text/html` |
| `display.markdown(content: string)` | `text/markdown` |
| `display.json(value: unknown)` | `application/json` |

### JupyterDisplay type

```ts
type JupyterDisplay = {
  readonly [K in typeof DISPLAY_SYMBOL]: () => Record<string, unknown>;
};
```

## ports

```ts
import { ports } from "@spur/app";
import type { PortData, PortEntry, PortManifest } from "@spur/app";
```

```ts
const data: PortData = await ports.read("spur-ad-capture");
// data.bytes       Uint8Array
// data.mime        string | undefined
// data.version     number
// data.kind        "arrow" | "media"
// data.durationSec number | undefined

const m: PortManifest = await ports.manifest();
// m.ports   Record<string, PortEntry>
```

The `ports` object exports exactly two functions: `ports.read(name, root?)` and
`ports.manifest(root?)`. There is no `ports.list()` or `ports.readManifest()`.

`ports.read(name, root?)` reads `SPUR_NOTEBOOK_PORT_ROOT` (or accepts an explicit
`root` override). The manifest path is `${root}/ports/manifest.json`. File path
is resolved via basename-join (never uses `entry.path` verbatim).

### PortEntry

```ts
interface PortEntry {
  path: string;          // versioned filename (e.g. "name@v2.media")
  version: number;
  kind: "arrow" | "media";
  mime?: string;
  size?: number;
  schema?: unknown;      // Arrow schema for kind:"arrow"
  duration_sec?: number; // seconds for kind:"media"
}
```

### PortData

```ts
interface PortData {
  bytes: Uint8Array;
  mime?: string;
  version: number;
  kind: "arrow" | "media";
  durationSec?: number;
}
```

Note: `duration_sec` in the manifest maps to `durationSec` in `PortData`
(camelCase conversion in the TypeScript SDK).

## Low-level wire primitives

`sdk/typescript/src/wire.ts` exports `readFrame`, `writeFrame`, `readExactly`
for direct use or for injecting fake connections in tests. These are not
re-exported from `mod.ts`; import directly when needed.
