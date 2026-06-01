---
name: notebook-data-app
description: "Use when the user asks to turn live data — a datasource catalog table, a REST/API table, read_json_auto/read_csv_auto over HTTP, or a file — into a working dashboard, monitor, or display app inside a Jute notebook. Establishes the reactive-DAG data pipeline (source cells → produces/consumes ports → a text/html artifact cell), single-kernel or cross-kernel (Python → Deno) via Arrow ports. Pairs with open-design for the visual layer."
role: brain
---
<!-- SPUR-MANAGED v=1 skill=notebook-data-app sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# Notebook Data App — Reactive DAG Pipeline + Display Artifact

You build a working data application *inside* a Jute notebook: source cells pull
live data, the reactive DAG wires them to a display cell, and the final artifact
renders as a `text/html` output. The notebook IS the app — pipeline and UI in one
document. This is the **data plane**; `open-design` is the **visual craft**. Use
both: this skill to make the data flow, `open-design` to make it look designed.

<HARD-GATE>
Operate the notebook ONLY through the `notebook_*` MCP tools (`notebook_insert_cell`,
`notebook_write_cell`, `notebook_read_cell`, `notebook_set_cell_metadata`,
`notebook_set_dag_metadata`, `notebook_set_cell_code_type`, `notebook_run_cell`,
`notebook_run_cascade`, `notebook_dag_status`, `notebook_push_source`,
`notebook_list_datasources`). Never ask the user to paste code or open files. The
final artifact MUST be a cell whose output carries `text/html`.
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

The artifact reads its inputs with `spur.get(port)` and emits one HTML document
(inline CSS + optional inline `<script>`; self-contained, no external assets).
- **Python cell:** `from IPython.display import HTML; HTML(html)`.
- **Deno cell** (set `code_type="javascript"`): `spur.get` returns an
  apache-arrow Table; finish with `await Deno.jupyter.display({"text/html": html}, {raw:true})`.

Build the HTML data-driven (rows, markers, bars derived from the ports) — never
hardcode values. For the visual direction, palette, layout specialism, and the
anti-slop critique, follow **open-design**.

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

## Common mistakes

| Symptom | Cause → Fix |
|---|---|
| `dag_status`/`run_cascade` "failed to build DAG" | One producer feeds 2+ ports to the same consumer (parallel edges → false cycle), or a real cycle. **Split each port into its own producer cell.** |
| Deno cell SyntaxError on `▸`/`const` in a `.py` traceback | Cell ran in the Python kernel. Set `notebook_set_cell_code_type(id,"javascript")`; run via `notebook_run_cascade` (routes JS to the `#deno` slot), not `notebook_run_cell`. |
| `read_json_auto` 429 / "unexpected end of data" | Endpoint rate-limits by IP or chunks the body. Use `urllib`+`json`, or route through a relay/proxy. |
| Artifact renders but values are stale/blank | It read globals, not ports, across a kernel boundary — or you never called `spur.put`. Wire via `spur.get`/`spur.put`. |
| `port_manifest` empty though cells "produce" | `produces` is only scheduling metadata; you must actually `spur.put`. |

## Reference
- `references/ports-and-kernels.md` — PortStore layout, `spur.put/get` contract, Deno slot + `code_type` routing, the parallel-edge invariant.
- **open-design** — visual direction, palette/font binding, surface specialism, five-dimensional + anti-slop critique for the artifact.
