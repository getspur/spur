---
name: notebook-data-app
description: "Use when the user asks to turn live data — a datasource catalog table, a REST/API table, read_json_auto/read_csv_auto over HTTP, or a file — into a working dashboard, monitor, or display app inside a Jute notebook, OR to build an interactive Spur App / Jute-App: a notebook that runs like an application, with frontend control cells (sliders, inputs) that recompute and re-render live in App mode. Covers the reactive-DAG data pipeline (source cells → produces/consumes ports → a text/html artifact), frontend cells, and the control→source.push→cascade loop, single-kernel or cross-kernel (Python → Deno) via Arrow ports. Pairs with open-design for the visual layer."
role: brain
---
<!-- SPUR-MANAGED v=1 skill=notebook-data-app sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# Notebook Data App — Reactive DAG Pipeline + Display Artifact

You build a working data application *inside* a Jute notebook: source cells pull
live data, the reactive DAG wires them to a display cell, and the final artifact
renders as a `text/html` output. The notebook IS the app — pipeline and UI in one
document. This is the **data plane**; `open-design` is the **visual craft**. Use
both: this skill to make the data flow, `open-design` to make it look designed.

Two altitudes:
1. **Display app** (steps 1–5 below) — sources → ports → one `text/html` artifact.
   A read-only dashboard/monitor. Start here; it is the foundation.
2. **Interactive Spur App** (the *“Make it an app”* section) — add **frontend
   cells**: view cells re-render when a port bumps, control cells push user input
   back into the DAG. Flip to **App mode** and the document behaves like a deployed
   app. Build the display app first, then promote it.

<HARD-GATE>
Operate the notebook ONLY through the `notebook_*` MCP tools (`notebook_insert_cell`,
`notebook_write_cell`, `notebook_read_cell`, `notebook_set_cell_metadata`,
`notebook_set_dag_metadata`, `notebook_set_cell_code_type`, `notebook_run_cell`,
`notebook_run_cascade`, `notebook_dag_status`, `notebook_push_source`,
`notebook_list_datasources`). Never ask the user to paste code or open files. The
final artifact MUST be a cell whose output carries `text/html`. Frontend-cell
declaration (Spur App) is `cell.metadata.spur.frontend`, set with
`notebook_set_cell_metadata` — NOT `notebook_set_dag_metadata` (that is `produces`/
`consumes` only).
</HARD-GATE>

## The loop

### 1. Source — one cell per data source, each publishes a port

Pull from the real substrate, in order of preference:
- **Datasource catalog** (`notebook_list_datasources`) — call the table function
  the gateway exposes, e.g. `duckdb.sql("SELECT ... FROM polymarket_markets()")`.
- **DuckDB over HTTP** — `read_json_auto(url)` / `read_csv_auto(url)` for free
  JSON/CSV/GeoJSON APIs (run `INSTALL httpfs; LOAD httpfs;` once).
- **`urllib` + `json`** — fallback for endpoints where DuckDB's HTTP reader 429s
  or truncates (rate-limited / chunked sources).

Shape each source into a tidy dataframe and publish it to a port:
```python
spur.put("quakes", q_df)   # writes Arrow IPC into the notebook PortStore
```
`spur` is auto-injected into every cell. `spur.put(port, df)` accepts a pandas
DataFrame / Arrow table / list / dict.

### 2. Wire — declare the DAG edges

For each source cell: `notebook_set_dag_metadata(id, {produces:[{port,repr:"arrow"}], consumes:[]}, expected_version)`.
For the artifact: `consumes:[<all ports>]`.

**The one rule that bites:** a producer cell may feed **at most one port to any
single consumer**. Two ports from the same producer to the same consumer are
parallel edges — split them into separate source cells. (See gotchas.)

Verify the graph built: `notebook_dag_status` → check `nodes`, `edges`, and
`port_manifest`. If it errors "failed to build DAG", you have a parallel edge or
a real cycle.

### 3. Transform (optional) — derived signals

A cell that `consumes` raw ports, computes metrics (aggregates, joins,
convergence/risk scores — SQL does most of this natively), and `produces` a
derived port the artifact consumes.

### 4. Artifact — consumes ports, renders `text/html`

The artifact reads its inputs with `spur.get(port)` and emits one `text/html` output.
- **Python cell:** `from IPython.display import HTML; HTML(html)`.
- **Deno cell** (set `code_type="javascript"`): `spur.get` returns an
  apache-arrow Table; finish with `await Deno.jupyter.display({"text/html": html}, {raw:true})`.

Build the HTML data-driven (rows, markers, bars derived from the ports) — never
hardcode values.

**Pick an artifact track — see open-design's `references/artifact-tracks.md`.** A
data app IS the dashboard / monitor surface those tracks were written for, so the
choice is load-bearing here, and it decides whether the artifact is self-contained:
- **Static / mostly-display app** → **Track B** (componentized + SSR on the Deno
  kernel) by default, or **Track A** (single-file HTML, pre-rendered DOM) for a
  simple read-out. Charts use **Observable Plot**, server-rendered to inline SVG.
  Self-contained: zero external URLs, reads correctly with scripts off.
- **Live / interactive / large-data dashboard** → **Perspective**
  (`<perspective-viewer>`, Datagrid + d3fc) — the default heavy-data visualizer for
  pivot/aggregate/filter and streaming `table.update()`. This is the **one accepted
  exception** to "self-contained": it loads WASM from a CDN and needs active content
  on, with no scripts-off baseline. Reach for it only when interactive exploration
  IS the product; otherwise stay on Plot.

For palette, font binding, layout specialism, and the anti-slop critique, follow
**open-design** (its `references/critique.md` self-contained gate applies to every
track except the Perspective dashboard exception above).

### 5. Run & verify

- `notebook_run_cell` each source (Python slot), then `notebook_run_cascade` from
  a source or the artifact — re-running a source cascades to the artifact.
- Confirm with `notebook_read_cell(artifact_id)` that the output mime is
  `text/html`, and `notebook_dag_status.port_manifest` shows each port versioned.

## Data plane: single-kernel vs cross-kernel

| | transport | when |
|---|---|---|
| **Single kernel** (all Python) | kernel globals **or** ports | simplest; cells share one process |
| **Cross-kernel** (e.g. Python sources → Deno artifact) | **Arrow ports only** (`spur.put`/`spur.get`) | a Deno cell is a separate process — Python globals are invisible; the ports ARE the only bridge |

`produces`/`consumes` metadata drives **scheduling/cascade order**, not data
transport. Data moves only when a cell calls `spur.put`/`spur.get`. See
`references/ports-and-kernels.md` for the on-disk port layout, the `spur` API, and
Deno kernel/slot mechanics.

## Make it an app — frontend cells, controls & App mode

A **Spur App** is the display app above plus **frontend cells** and **App mode**.
The DAG/ports pipeline is unchanged; you add two-way reactivity on top of it.

### Declare a frontend cell

A cell becomes a frontend cell when it carries `cell.metadata.spur.frontend`
(set with `notebook_set_cell_metadata`, with `expected_version`):
```jsonc
{ "spur": { "frontend": {
  "kind": "chart",          // chart | table | slider | form | html | custom (advisory)
  "binds": ["forecast"],    // input ports → render; re-renders on port bump
  "emits": ["horizon"],     // user actions → source.push to these ports
  "props": { "x": "month", "y": "revenue" }
} } }
```
Two flavors (the normalizer keeps only `binds`/`emits` as string lists):
- **View cell** — `binds`, no `emits`. Pure output (chart/table/KPI). Re-renders
  when a bound port's manifest version changes; needs no kernel to re-render.
- **Control cell** — `emits`. A slider/input/button. User action → `source.push`
  to the named port → the engine cascades downstream → bound view cells re-render.
  **That closed loop is what makes it an app.**

### The reactive loop (control → cascade → re-render)

```
user input → AFM widget model.save_changes()/model.send()
  → host maps it to a source.push on the declared `emits` port (allowlisted)
  → ReactiveEngine cascades stale downstream cells in topological order
  → produced ports bump in manifest.json
  → bound view cells re-render
```
Author a control as a **Deno cell** with `@anywidget/deno`’s
`widget({ state, render })`; the frontend owns the widget, the kernel just declares
the binding and initial state. The widget changing model state is what emits the
intent; only intents that map to a **declared `emits` source port** are turned into
a `source.push` (others are rejected unless backend-allowlisted).

### App mode

Title-bar segmented toggle **`Notebook | DAG | App`**. **App** hides code and shows
**only frontend cells**, *chromeless* (output only — no code, run buttons, or DAG
badges), as a **vertical stack in document order**, under a counts-only status strip
(`App · N frontend cells · running/failed/stale`). Marking a cell `spur.frontend`
is all it takes to make it appear there.

### Reality check — do NOT overclaim

The loop runs over the **in-process** comm/Tauri path. As built today there is:
**no `ipc://` cell bus, no multi-client, no headless runner, no grid layout**
(document-order stack only; `props.layout` is not honored). A dead kernel **is**
auto-restarted (heartbeat supervisor) and view cells rehydrate last values from the
manifest — but don’t promise bus/remote/multi-window behavior the code doesn’t have.

## Common mistakes

| Symptom | Cause → Fix |
|---|---|
| `dag_status`/`run_cascade` "failed to build DAG" | One producer feeds 2+ ports to the same consumer (parallel edges → false cycle), or a real cycle. **Split each port into its own producer cell.** |
| Deno cell SyntaxError on `▸`/`const` in a `.py` traceback | Cell ran in the Python kernel. Set `notebook_set_cell_code_type(id,"javascript")`; run via `notebook_run_cascade` (routes JS to the `#deno` slot), not `notebook_run_cell`. |
| `read_json_auto` 429 / "unexpected end of data" | Endpoint rate-limits by IP or chunks the body. Use `urllib`+`json`, or route through a relay/proxy. |
| Artifact renders but values are stale/blank | It read globals, not ports, across a kernel boundary — or you never called `spur.put`. Wire via `spur.get`/`spur.put`. |
| `port_manifest` empty though cells "produce" | `produces` is only scheduling metadata; you must actually `spur.put`. |
| Frontend cell doesn't show in App mode | Missing `cell.metadata.spur.frontend`. Set it with `notebook_set_cell_metadata` (not `set_dag_metadata`). |
| Control moves but nothing recomputes | The control's `emits` port isn't declared, the AFM intent doesn't map to a declared source port, or there's no consumer wired to that port. Declare `emits:[port]` and `consumes:[port]` downstream. |
| Privileged action (shell/secret) wired to a control | Frontend emits **intent only**; privileged actions must be backend-allowlisted and confirmed. Never let a control run shell or receive raw secrets. |

## Reference
- `references/ports-and-kernels.md` — PortStore layout, `spur.put/get` contract, Deno slot + `code_type` routing, the parallel-edge invariant.
- `docs/superpowers/specs/2026-06-01-jute-app-notebook-as-application-container-design.ipynb` — the Jute-App architecture (three layers, frontend cells, reactive loop, App mode, supervision §8). Read for the *why* behind the `spur.frontend` contract; mind the §5–§9 migration status (the `ipc://` bus is designed but not built).
- **open-design** `references/artifact-tracks.md` — Track A/B build pipeline, the Plot-vs-Perspective chart decision, and the self-contained gate. The artifact track for any data app.
- **open-design** — visual direction, palette/font binding, surface specialism, five-dimensional + anti-slop critique for the artifact.
