# Creation log — notebook-data-app

## Provenance
Distilled from a verified end-to-end session that built a "World Monitor" replica
dashboard inside a Jute notebook:
- Sources: `polymarket_markets()` (datasource catalog / spur_rest gateway),
  USGS earthquakes (`read_json_auto` over httpfs), Hacker News (`urllib`+`json`
  after `read_json_auto` hit truncation/429), Natural Earth basemap (fetch).
- Wired as a reactive DAG (`produces`/`consumes` ports), rendered as a `text/html`
  artifact — first in a single Python kernel (globals), then cross-kernel with a
  Deno artifact reading Arrow ports via `spur.get` (`port_manifest` populated;
  `[deno artifact] read ports → markets:5 agg:100 quakes:8 news:6`).

## Bugs/gotchas this skill encodes (all hit live)
- **Parallel-edge false cycle**: one producer feeding two ports to one consumer
  made `topological_sort` report a cycle → `dag_status`/`run_cascade` failed.
  Root-caused via `code_*` + spur-analyst; fixed in
  `spur-notebook/src/dag/graph.rs::topological_sort` (commit on `main`). Skill
  still advises distinct producers since a running daemon may predate the fix.
- **Deno routing**: a JS artifact ran in the Python kernel (SyntaxError on `▸`)
  until `code_type` was set to `javascript`; JS runs in the `#deno` slot and must
  go through `run_cascade`.
- **httpfs fragility**: GDELT 429'd, HN truncated — `urllib` fallback / relay.

## Integration: open-design artifact-tracks (2026-06-04)
Step 4 (Artifact) and the Reference section now point at open-design's
`references/artifact-tracks.md` and fold in the data-app-relevant decision: Track A
(single-file HTML) vs Track B (componentized + SSR) for static/display apps with
Observable Plot charts, vs **Perspective** for live/large-data dashboards. This
also reconciled a contradiction: the skill previously stated "self-contained, no
external assets" as an absolute, but a Perspective dashboard — the skill's headline
"dashboard/monitor" use case — is deliberately NOT self-contained (CDN WASM, active
content on). That exception is now named in both SKILL.md step 4 and
`references/ports-and-kernels.md` so the two docs no longer disagree. The
artifact-tracks content stays owned by open-design (cross-referenced, not copied) to
avoid drift; only the data-plane-facing decision is summarized inline.

## Interactive Spur App section (2026-06-06)
Added a "Make it an app — frontend cells, controls & App mode" section plus an
altitude split in the intro (display app vs interactive Spur App), broadened the
description triggers, extended the HARD-GATE (frontend metadata is
`cell.metadata.spur.frontend` via `notebook_set_cell_metadata`, not
`set_dag_metadata`), and added three Common-mistakes rows (frontend cell not in App
mode; control moves but no recompute; privileged action wired to a control).

Grounded in a deep audit of the as-built implementation at commit `79d2a0b4`
(read via `code_*` + direct reads): `AppMode.tsx` (chromeless vertical stack +
counts-only status strip), `NotebookHeader.tsx` (the `Notebook|DAG|App` segmented
toggle + `NotebookViewMode`), `notebook.ts::frontendMetadataFromSpur` /
`normalizeFrontendMetadata` (the `spur.frontend` shape; only `binds`/`emits` kept as
string lists), and `afmHost.ts` (`model.save_changes()`→`model-state.update`,
`model.send()`→`model-state.custom`; intents mapped to a declared `emits` source
port become a `source.push`, others rejected unless allowlisted). The control→
cascade loop and supervision claims match the kernel-supervision slice landed this
session (heartbeat auto-restart, merge `54ab1b41`). A reproduced App-mode UI
artifact (scratch notebook) confirmed the surface visually.

The "Reality check — do NOT overclaim" subsection is deliberate: the audit found the
loop runs in-process (no `ipc://` bus / multi-client / headless / grid layout). The
skill must not teach an agent to promise those.

## Testing caveat
Authored as a reference/technique skill grounded in the above verified session
rather than fresh RED-GREEN subagent baselines. Per `writing-skills`, run pressure
scenarios with subagents (can they build a 2-source → artifact app cross-kernel
without hitting the parallel-edge trap, and pick Perspective vs Plot correctly for a
live vs static app?) before treating it as bulletproof. The artifact-tracks
integration above is likewise an unverified cross-reference edit, not a tested one.
