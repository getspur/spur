# Notebook MCP tool surface (inventory)

Live MCP evaluation (2026-08): **64 tools**. Names are MCP tool basenames (`notebook__` prefix omitted in prose as `notebook_*`).

Use this as a lookup; the parent skill owns protocol and routing.

## Lifecycle (12)

| Tool | Purpose |
|---|---|
| `notebook_ping` | Socket smoke test |
| `notebook_new` | Create Untitled scratch + open |
| `notebook_open` | Open path; flushes prior buffer; sets `active_path` |
| `notebook_close` | Flush + close window |
| `notebook_reopen` | Reopen last known window |
| `notebook_reload` | Re-read open path from disk (dispositions: loaded/reused/reloaded/kept_dirty) |
| `notebook_list_recents` | Recent notebooks, newest first |
| `notebook_set_pinned` | Pin/unpin in recents |
| `notebook_remove_from_recents` | Forget path (no delete) |
| `notebook_move_to_trash` | OS trash; refuses active notebook |
| `notebook_reveal_in_finder` | Reveal in file manager |
| `notebook_discard_scratch` | Trash inactive scratch notebooks |

## Inspect / orient (9)

| Tool | Purpose |
|---|---|
| `notebook_context_pack` | **First** orientation pack + `next_queries` (active open notebook) |
| `notebook_snapshot` | Truncated cell previews of active notebook |
| `notebook_get_notebook` | Full notebook document from disk by path |
| `notebook_read_cell` | Full source + outputs for one loaded cell |
| `notebook_dag_status` | DAG nodes/edges + port manifest versions (active) |
| `notebook_catalog` | Datasource catalog layer hop (`ds://`); `scope=all\|used` |
| `notebook_lineage` | Upstream/downstream from `ds://` / `cell://` / `port://` |
| `notebook_symbol_search` | Live symbol facts → `sym://` refs |
| `notebook_symbol_refs` | Define site, ports, declared-vs-actual drift, port co-users |

## Mutate (9)

| Tool | Purpose |
|---|---|
| `notebook_insert_cell` | Insert code/markdown after `after_id`; needs `mutation_id`; code requires `code_type` |
| `notebook_write_cell` | Full source replace; `expected_version` + writer target |
| `notebook_edit_cell` | Targeted string replacements; prefer for large cells |
| `notebook_delete_cell` | Delete with `mutation_id` + `expected_version` |
| `notebook_set_cell_metadata` | Patch `jute_deck` / `spur` frontend & related cell metadata |
| `notebook_set_cell_code_type` | Metadata-only language: python/javascript/rust/go/sql/ns_mermaid |
| `notebook_set_dag_metadata` | `produces` / `consumes` / `source` wiring |
| `notebook_set_schedule` | Cell cron trigger or clear (`trigger=null`) |
| `notebook_save` | Persist full document to path (`force` for empty overwrite) |

Writer-target tools require `notebook_path` **or** `notebook_id`. Non-writer → `wrong_notebook`.

## Run (3)

| Tool | Purpose |
|---|---|
| `notebook_run_cell` | Run one cell; mark downstream stale (no cascade execute) |
| `notebook_run_cascade` | Run cell + cascade downstream through reactive engine |
| `notebook_push_source` | Push Arrow IPC bytes into a source port + queue engine |

## Kernel / venv (9)

| Tool | Purpose |
|---|---|
| `notebook_start_kernel` | Start kernelspec (`python3`, `deno`, `evcxr`, `gonb`; js/ts → deno) |
| `notebook_stop_kernel` | Stop + clear slot |
| `notebook_restart_kernel` | Restart existing slot |
| `notebook_interrupt` | Interrupt running kernel |
| `notebook_kernel_info` | Status, generation, resource usage |
| `notebook_venv_list` | Managed Python venvs |
| `notebook_venv_create` | Create venv for python_version |
| `notebook_venv_delete` | Delete venv by id |
| `notebook_venv_list_python_versions` | Available managed CPython versions |

## API / datasources (10)

| Tool | Purpose |
|---|---|
| `notebook_list_datasources` | Active notebook catalog list |
| `notebook_add_api_datasource` | Built-in table-fn sources (`polymarket`, `rss`) |
| `notebook_add_api_connection` | REST connection (no credential values) |
| `notebook_list_api_connections` | Slim cards; `detail=full` only if needed |
| `notebook_list_api_providers` | Provider cards; prefer navigate |
| `notebook_navigate_api_connections` | Search/hop connections + tables |
| `notebook_navigate_api_providers` | Search/hop providers |
| `notebook_api_connection_status` | One connection status + table-fn index |
| `notebook_preview_api_tables` | Preview tables from OpenAPI text |
| `notebook_oauth_connect` | Browser OAuth for saved connection |

## Spur App package (6)

| Tool | Purpose |
|---|---|
| `notebook_app_briefing` | Canonical app-agent briefing (call first for app work) |
| `notebook_app_init` | Scaffold app_root from template |
| `notebook_app_doctor` | Conformance (`static` or `full`) |
| `notebook_app_pack` | Doctor then pack `.spurapp` |
| `notebook_export_spur_app` | Export `.ipynb` → `.spurapp` |
| `notebook_import_spur_app` | Import `.spurapp`; optional open |

Templates: `minimal`, `frontend-only`, `headless`, `react-widget`, `interactive-decoupled`.

## Open Design (2)

| Tool | Purpose |
|---|---|
| `open_design_search` | Search design systems / deck themes |
| `open_design_get` | Fetch one system/theme by id |

## NS-Mermaid (3)

| Tool | Purpose |
|---|---|
| `notebook_ns_mermaid_spec` | Capability registry (profiles, proof kinds, examples) |
| `notebook_ns_mermaid_check` | Read-only parse/bind/typecheck/obligation preview (no solver publish) |
| `notebook_ns_mermaid_explain` | Explain diagnostic / obligation / counterexample / timeout |

## Code (1)

| Tool | Purpose |
|---|---|
| `code_semantic_search` | BM25 semantic search over analyst index (docs + code tokens) |

## Error codes to treat as protocol signals

| Signal | Action |
|---|---|
| `notebook_not_open` | `notebook_open` / `notebook_new` first |
| `wrong_notebook` | Target writer-owned path/id; open that notebook if needed |
| version conflict | Re-`read_cell`, retry with new `expected_version` |
| `mutation_id_conflict` | New id for a different intent; identical params replay receipt |
| `daemon_unavailable` | Notebook daemon not up; user/TUI must host MCP socket |

## Specialist ownership (do not reimplement here)

| Family / concern | Skill |
|---|---|
| Ports, frontend cells, Deno AFM apps | `notebook-data-app` |
| Visual HTML craft loop | `open-design` |
| Deck / Present / jute_deck | `jute-deck-mode` |
| App authoring loop beyond MCP names | `notebook_app_briefing` → SDK skill pointers |
