# Open Design — Artifact Tracks (A vs B)

Two ways to produce the single `text/html` artifact cell. Both must pass the
self-contained + scripts-off gate in `references/critique.md`.

## Decision rule

- **Track B (componentized + SSR) is the default.** Use it for anything interactive,
  stateful, or likely to grow: dashboards, prototypes, multi-panel tools, the DAG view.
  It scales (real components, not one growing `innerHTML` string) and it degrades
  correctly when scripts are off, because the static baseline is server-rendered.
- **Track A (single-file hand-written HTML) is for simple / basic / mostly-static
  artifacts:** a poster, a one-section landing mock, a static infographic. It is smaller
  and dependency-free, but it does not scale and it is easy to get the scripts-off gate
  wrong with it (see below).
- **Size is not the deciding factor.** Track B ships a component runtime (Preact ~16KB
  min) plus compiled CSS; a full Track B artifact lands around 50-60KB. That is fine.
  Choose on scale and interactivity, not bytes.

## Hard constraints (both tracks)

These are enforced by `references/critique.md`; repeated here because the build step makes
them easy to violate:

1. **Self-contained.** The rendered HTML has zero external resource URLs: no
   `<script src>`, no CDN stylesheet, no network-loaded module, no remote-only font. A
   kernel build step is allowed, but it must inline everything it pulled in.
2. **Meaningful with scripts off.** Active content is off by default, so the static markup
   must carry the design on its own. Interactivity is progressive enhancement.

## Track A — single-file HTML

Hand-write one document: inline `<style>`, markup, optional inline `<script>`. Python or
Deno kernel, whichever is open (`IPython.display.HTML(...)` or the Jupyter.display symbol).

Pitfall that fails the gate: building the whole UI in the inline script (empty containers
filled by a `render()` call on load). With scripts off that renders blank. If you use
Track A for anything with structure, **pre-render the initial DOM into the HTML** and let
the script only enhance it.

## Track B — componentized + SSR (default)

Kernel: **Deno** (`spec_name: "deno"`, TypeScript). Pipeline, all inside one code cell:

1. **Author a stateless shared view** (`flow_view.ts`): components built with Preact `h(...)`,
   no hooks. State is passed in as props with optional callbacks.
2. **SSR the baseline** with `preact-render-to-string` (the static markup, scripts-off view).
3. **Compile CSS in-kernel** (e.g. Tailwind v3 via `postcss`), scanning the rendered markup
   so only used utilities ship. Inline the result.
4. **Inline-bundle the client island** with esbuild: a small entry that imports the same
   view plus `preact/hooks`, holds state, and `hydrate`s. Bundle to a minified IIFE.
5. **Assemble one document**: inline `<style>` + SSR markup + inline `<script>`. Emit one
   `text/html` output. Re-read the cell and confirm zero external URLs.

### Three gotchas (learned the hard way)

- **Two Preact instances.** `preact-render-to-string` and a separately-imported
  `preact/hooks` resolve to different module instances, so using hooks during SSR throws
  `Cannot read properties of undefined (reading '__H')`. Keep SSR hooks-free (stateless
  view); put all hooks in the client island, which esbuild bundles into one instance.
- **jsr.io may be blocked** in headless / sandboxed runs (403), so `@luca/esbuild-deno-loader`
  can fail to load. Hand-roll a tiny esbuild http-import plugin (resolve `https?://`
  specifiers, fetch their contents) instead. It needs only `npm:esbuild`.
- **Active content default off.** Verify the SSR baseline alone reads as the design before
  shipping; the bundled script is enhancement, not the artifact.

### Recipe skeleton

```ts
import { render as ssr } from "https://esm.sh/preact-render-to-string@6.5.11";
import { h } from "https://esm.sh/preact@10.24.3";
import * as esbuild from "npm:esbuild@0.24.0";
import postcss from "npm:postcss@8.4.49";
import tailwindcss from "npm:tailwindcss@3.4.17";

const viewSrc = `/* stateless components: export function View(props){...}, export const initial=()=>({...}) */`;
const clientSrc = `import { hydrate, h } from "https://esm.sh/preact@10.24.3";
import { useState } from "https://esm.sh/preact@10.24.3/hooks";
import { View, initial } from "./flow_view.ts";
function App(){ const [s,set]=useState(initial()); /* handlers */ return h(View,{...s, /* callbacks */}); }
hydrate(h(App,null), document.getElementById("app"));`;
await Deno.writeTextFile("/tmp/flow_view.ts", viewSrc);
await Deno.writeTextFile("/tmp/flow_client.ts", clientSrc);

const mod = await import("file:///tmp/flow_view.ts?v=" + Date.now());
const ssrHtml = ssr(h(mod.View, { ...mod.initial() }));          // no hooks here

const tw = await postcss([tailwindcss({
  content: [{ raw: ssrHtml, extension: "html" }, { raw: viewSrc, extension: "js" }],
  corePlugins: { preflight: true },
})]).process("@tailwind base;@tailwind utilities;", { from: undefined });
const css = tw.css; // append any @keyframes you need (Tailwind has none for custom motion)

const httpPlugin = { name: "http", setup(b) {
  b.onResolve({ filter: /^https?:\/\// }, (a) => ({ path: a.path, namespace: "http-url" }));
  b.onResolve({ filter: /.*/, namespace: "http-url" }, (a) => ({ path: new URL(a.path, a.importer).toString(), namespace: "http-url" }));
  b.onLoad({ filter: /.*/, namespace: "http-url" }, async (a) => ({ contents: await (await fetch(a.path)).text(), loader: "js" }));
} };
const built = await esbuild.build({ plugins: [httpPlugin], entryPoints: ["/tmp/flow_client.ts"],
  bundle: true, minify: true, format: "iife", write: false, legalComments: "none" });
await esbuild.stop();
const clientJs = built.outputFiles[0].text;

const doc = "<!doctype html><html><head><meta charset=utf-8><style>" + css +
  "</style></head><body style=\"margin:0\"><div id=app>" + ssrHtml + "</div><script>" + clientJs + "</script></body></html>";
// emit: ({ [Symbol.for("Jupyter.display")]() { return { "text/html": doc }; } })
```

Dynamic, data-driven values (positions, status colors) stay inline `style`; structural
styling is utility classes so Tailwind can tree-shake them. Custom motion (pulse, dashed
edges) is a small appended `@keyframes` block, since Tailwind ships none by default.

## Charts and data viz

Two defaults, by job: static charts use Plot; dashboards use Perspective.

- **Static charts: Observable Plot, server-rendered to inline SVG (default for static / embedded charts).** Plot is code-first (compose
  layered marks in one spec). In the Deno kernel, give it a DOM via a jsdom shim and read
  `chart.outerHTML` to get a static `<svg>` string, then inline it. The chart renders with
  scripts OFF and ships zero external URLs (jsdom and Plot are kernel-time only, never
  shipped). This is the chart-shaped version of the Track B "render static at kernel time"
  rule, and the output is tiny (single-digit KB of SVG).

  ```ts
  import * as Plot from "npm:@observablehq/plot";
  import { JSDOM } from "npm:jsdom";
  const dom = new JSDOM("<!doctype html><html><body></body></html>");
  globalThis.document = dom.window.document;
  globalThis.window = dom.window;
  const svg = Plot.plot({ marks: [/* areaY + lineY + dot + ruleY ... */] }).outerHTML;
  // inline `svg` into the one self-contained text/html document
  ```

  Need interactivity (hover, brush, click)? Hydrate a Plot/Preact client island via the
  Track B bundle path, and keep the SSR'd SVG as the scripts-off baseline.

- **Dashboards / heavy / interactive / streaming data: Perspective (default dashboard visualizer).**
  `@finos/perspective` is a C++/WASM analytics engine with a `<perspective-viewer>` web component
  (Datagrid + d3fc chart plugins). Use it for the `dashboard` surface and any artifact whose point is
  large-data exploration, pivot/aggregate/filter, or live updates. Verified in the Jute iframe (500k
  rows, live `table.update()` streaming, d3fc charts). It is NOT self-contained: it loads ~2.5 MB of
  WASM from a CDN and needs active content ON, with no scripts-off baseline. That tradeoff is accepted
  for dashboards (interactivity is the product); use Plot instead when a static self-contained chart
  will do. Build it from a Deno cell that emits the document:

  ```ts
  const V = "3.8.0";
  const doc = `<!doctype html><html><head><meta charset=utf-8>
  <link rel="stylesheet" crossorigin="anonymous" href="https://cdn.jsdelivr.net/npm/@finos/perspective-viewer@${V}/dist/css/themes.css">
  <style>body{margin:0}#v{display:block;height:520px}</style></head><body>
  <perspective-viewer id="v"></perspective-viewer>
  <script type="module">
  const perspective = (await import("https://cdn.jsdelivr.net/npm/@finos/perspective@${V}/dist/cdn/perspective.js")).default;
  await import("https://cdn.jsdelivr.net/npm/@finos/perspective-viewer@${V}/dist/cdn/perspective-viewer.js");
  await import("https://cdn.jsdelivr.net/npm/@finos/perspective-viewer-datagrid@${V}/dist/cdn/perspective-viewer-datagrid.js");
  await import("https://cdn.jsdelivr.net/npm/@finos/perspective-viewer-d3fc@${V}/dist/cdn/perspective-viewer-d3fc.js");
  const worker = await perspective.worker();
  const table = await worker.table(DATA /* , { index: "id" } to enable in-place streaming */);
  const v = document.getElementById("v"); await v.load(table);
  await v.restore({ plugin: "Datagrid", theme: "Pro Light", settings: true /*, group_by, split_by, columns, aggregates, sort */ });
  // streaming: setInterval(() => table.update(BATCH), 150);  // indexed rows update in place, viewer auto-redraws
  </script></body></html>`;
  // emit: ({ [Symbol.for("Jupyter.display")]() { return { "text/html": doc }; } })
  ```

  Two non-obvious gotchas (each cost real debugging):
  1. **d3fc chart plugins require a theme.** Pass `theme: "Pro Light"` (or `"Pro Dark"`) in `restore`,
     or any chart plugin throws `Cannot read properties of null (reading 'opacity')`. Datagrid is unaffected.
  2. **The themes.css `<link>` must be `crossorigin="anonymous"`.** d3fc reads the theme from the
     stylesheet's `cssRules`; a cross-origin sheet without the attribute is unreadable (SecurityError),
     so the theme resolves to null and you get the same crash plus a follow-on `View not found`. jsdelivr
     sends `Access-Control-Allow-Origin: *`, so the attribute alone fixes it (and works in the
     opaque-origin sandbox too). Bulletproof alternative: inline the theme CSS as a same-origin `<style>`.
