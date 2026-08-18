# Ports & Kernels — the data-plane contract

## PortStore on disk

Ports are versioned Arrow IPC files, one per write, with a `manifest.json`
tracking the live version per port:

```
~/.spur/notebooks/<notebook_id>/ports/<port>@v<version>.arrow
~/.spur/notebooks/<notebook_id>/ports/manifest.json
```

`<notebook_id>` = `nb-` + first 24 hex of blake3(absolute notebook path). Python
and Deno cells in the **same notebook** resolve the **same** port dir, so a port
written by Python is readable by Deno and vice-versa.

## The injected `spur` helper

Every cell is wrapped with a `spur` object before execution (the single
chokepoint wraps Python and Deno cells alike — you do not import it):

- **Python:** `spur.put(port, value)` writes Arrow IPC (pandas DataFrame / numpy /
  pyarrow Table / dict / list). `spur.get(port)` reads → pandas DataFrame.
  Requires `pyarrow` in the kernel.
- **Deno/JS:** `globalThis.spur.put(port, value)` / `spur.get(port)` use
  `npm:apache-arrow`. `spur.get` returns an Arrow `Table`; iterate via
  `t.numRows` + `t.getChild(name).get(i)` (int64 columns come back as `bigint` —
  `Number(...)` them).

`produces`/`consumes` DAG metadata only schedules re-runs; it never moves bytes.
Data moves exactly when a cell calls `spur.put`/`spur.get`. `notebook_push_source`
is the external/cross-process way to inject Arrow into a declared **source** port.

## Rendering the artifact

- Python: `from IPython.display import HTML; HTML(html_string)`.
- Deno: `await Deno.jupyter.display({ "text/html": html }, { raw: true })`.

Keep the HTML self-contained (inline CSS + inline `<script>`); the cell output is
rendered in Jute's sandboxed iframe. The one sanctioned exception is a Perspective
dashboard (CDN WASM, active content on) — see SKILL.md step 4 and open-design's
`references/artifact-tracks.md` for the track decision.

## Kernels & slots

- Each notebook has a base kernel slot `notebook:<path>` and language-specific
  slots; **JavaScript/TypeScript cells run in a separate `notebook:<path>#deno`
  slot** (a distinct Deno process). Rust is not runnable yet.
- A cell's language comes from `code_type` (`python` | `javascript` | `rust`).
  `notebook_insert_cell(code_type=...)` may not persist it for routing — set it
  explicitly with `notebook_set_cell_code_type(id, "javascript", expected_version)`.
- `notebook_run_cell` targets the base slot; for a JS cell use
  `notebook_run_cascade`, which routes by `code_type` to the `#deno` slot and runs
  downstream consumers. First Deno run downloads `npm:apache-arrow` — allow time.
- Cross-kernel data flow is therefore: Python `spur.put(arrow)` → PortStore →
  Deno `spur.get(arrow)`. No shared namespace exists across the process boundary.

## The parallel-edge invariant (why "failed to build DAG" happens)

The reactive DAG runs a Kahn topological sort over producer→consumer edges. A
producer cell that emits **multiple ports all consumed by the same consumer**
creates parallel edges between one node pair. Keep each consumer's inbound ports
sourced from **distinct producer cells** — e.g. don't have one cell produce both
`markets` and `markets_agg` if a single artifact consumes both; split the
aggregate into its own source cell. (A correctness fix for the parallel-edge case
lives in `spur-notebook/src/dag/graph.rs::topological_sort`; the running daemon
must be rebuilt to pick it up, so prefer distinct producers regardless.)

## Minimal cross-kernel example

```python
# Python source cell  (DAG: produces ["quakes"])
import duckdb, pandas as pd
duckdb.sql("INSTALL httpfs; LOAD httpfs;")
df = duckdb.sql("""
  SELECT f.properties.place AS place, CAST(f.properties.mag AS DOUBLE) AS mag
  FROM (SELECT unnest(features) f
        FROM read_json_auto('https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/4.5_day.geojson'))
  WHERE f.properties.mag IS NOT NULL ORDER BY mag DESC LIMIT 8
""").df()
spur.put("quakes", df)
```

```javascript
// Deno artifact cell  (code_type=javascript; DAG: consumes ["quakes"])
const t = await spur.get("quakes");
const rows = Array.from({length: t.numRows}, (_, i) => ({
  place: t.getChild("place").get(i), mag: t.getChild("mag").get(i),
}));
const html = `<!doctype html><meta charset=utf-8><ul>` +
  rows.map(r => `<li>M${r.mag.toFixed(1)} — ${r.place}</li>`).join("") + `</ul>`;
await Deno.jupyter.display({ "text/html": html }, { raw: true });
```
