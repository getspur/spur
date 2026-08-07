---
name: notebook-mcp
description: "Use when operating, inspecting, editing, running, or navigating a SPUR/Jute notebook through notebook_* MCP tools — before reading .ipynb from disk, asking the user to paste cell code, guessing active_path, or calling cell mutations without expected_version / writer ownership."
role: both
---
<!-- SPUR-MANAGED v=1 skill=notebook-mcp sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# Notebook MCP — Operate Notebooks Through Tools

The notebook daemon owns the open document, kernels, DAG, ports, and recents. Drive it with `notebook_*` tools only. Inspect binds to **active** `current_path`; mutations require the **writer-owned** session.

Live surface: **64 tools / 10 families** (`references/tool-surface.md`). Ownership (Z3 sat `sol_05c0e55474144d27`): this skill = orient/mutate/run/catalog-nav/kernel/lifecycle protocol; craft → specialists.

<HARD-GATE>
1. **Orient first:** open notebook → `notebook_context_pack` before answering from state or mutating. Follow `next_queries`.
2. **No raw `.ipynb` authoring** and no "paste this cell" — use `insert_cell` / `write_cell` / `edit_cell`.
3. **Writer + version:** mutations that take `notebook_path`/`notebook_id` must hit the writer session (`wrong_notebook` otherwise). Pass current `expected_version` (≥1); on conflict re-read and retry.
4. **Active ≠ writer:** `notebook_open` sets inspect focus. Mutations do not switch focus.
</HARD-GATE>

## Classify → tools

| Question | Sequence |
|---|---|
| Orient / what's here? | `open` if needed → **`context_pack`** → follow refs |
| Full cell / doc | `snapshot` (truncated) → `read_cell`; or `get_notebook(path)` |
| Data / symbol provenance | `catalog` / `lineage` / `symbol_search` → `symbol_refs` |
| Add or change cells | writer target + version → `insert` / `write` / `edit` / `delete` (+ `mutation_id` on insert/delete) |
| Run / refresh DAG | `run_cell` (one; marks stale) or `run_cascade` (recomputes downstream) |
| Ports / language / schedule | `set_dag_metadata` / `set_cell_code_type` / `set_schedule` / `set_cell_metadata` |
| API / datasources | prefer `navigate_api_*` over full dumps → add / status / `oauth_connect` |
| Kernel / venv | `kernel_info` → interrupt/restart/start; js/ts → deno |
| Spur App package | **`app_briefing` first** → init / doctor / pack |
| Visual / deck / data app | escalate **open-design** / **jute-deck-mode** / **notebook-data-app** |
| NS-Mermaid | `spec` → `check` (no publish) → `run_cell` to prove; `explain` on errors |
| Repo code concept | `code_semantic_search` or **code-explore** — not cell tools |

## Core model

| Concept | Rule |
|---|---|
| **active** | Daemon path after open/new; powers pack/snapshot/dag/catalog/lineage |
| **writer session** | Only writable store; pass matching path or id |
| **expected_version** | Optimistic concurrency; always re-read after prior writes |
| **mutation_id** | Idempotent insert/delete; same params replay receipt; mismatch → conflict |
| **refs** | `ds://` `cell://` `port://` `sym://` — carry, never invent |
| **DAG vs transport** | `produces`/`consumes` schedule only; data via `spur.put`/`get` or `push_source` |

## Mutation loop

```
open(path) → context_pack / read_cell (versions)
→ mutate(path|id, expected_version, [mutation_id])
→ on conflict: re-read, never force
→ run_cell | run_cascade → read_cell + dag_status
```

Prefer `edit_cell` for small patches on large cells; `write_cell` for short/full rewrites. Code insert **requires** `code_type`; markdown must omit it. Do not `write_cell` right after `insert_cell` when insert already had the source.

## Families + escalate

Full inventory: `references/tool-surface.md`.

- **Orient:** context_pack, snapshot, read_cell, dag_status, catalog, lineage, symbol_*
- **Mutate / run:** insert/write/edit/delete, set_*, save; run_cell, run_cascade, push_source
- **Kernel / lifecycle / API:** start/stop/restart, venv_*; open/new/reload/recents; navigate_api_*, add_api_*
- **App / design / NS / code:** app_briefing→init/doctor/pack; open_design_*; ns_mermaid_*; code_semantic_search

Craft skills (do not restate): **notebook-data-app** (ports/AFM), **open-design** (visual HTML), **jute-deck-mode** (slides), **code-explore** (repo graph). Spur Apps: `app_briefing` first.

## Anti-patterns

| Excuse | Reality |
|---|---|
| Read the `.ipynb` file | Buffer may lead disk — open + pack/read_cell |
| Skip expected_version / run_cell = whole DAG | Version required; cascade to recompute |
| ns_mermaid_check / list_api full / open_design tools alone | Check≠prove; navigate>dump; load craft skills |
## TL;DR

```
0. open → context_pack → follow next_queries/refs
1. Mutate writer-owned notebook with expected_version (+ mutation_id)
2. edit_cell patches; write_cell rewrites; re-read on conflict
3. run_cell one node; run_cascade reactive recompute
4. navigate_api_* / catalog / lineage / symbol_* for discovery
5. Craft → notebook-data-app | open-design | jute-deck-mode | app_briefing
6. Never raw .ipynb authoring as a notebook_* substitute
```
