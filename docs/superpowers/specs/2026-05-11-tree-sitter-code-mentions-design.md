# Tree-sitter Code Mentions Design

**Status:** design approved
**Date:** 2026-05-11
**Owner:** Kevin Truong
**Related decisions:**
- `docs/spur/graphify-rust-duckdb-onager-alternative.md`
- `bd-2hh` Graphify operational graph architecture decision
- `749ad09e docs(spur): bd-3bm define graph agent queries`

## 1. Goal

Add a code-graph-backed `@` mention source so users can mention files and symbols precisely from the TUI input bar. The mention picker should use the prebuilt Graphify Phase 1 graph index, not live parsing, so accepted mentions carry exact entity identity, file path, and source range.

The first version exists to make prompts more precise:

- users type `@` and select a file or symbol;
- SPUR inserts an atomic mention into the input bar;
- the hidden payload points to a stable graph entity and its source span;
- on submit, SPUR can expand the mention into one exact read target for the brain or worker agent;
- if the agent needs topology beyond that target, it calls graph MCP tools such as `get_callers`, `get_callees`, or `get_subgraph`.

## 2. Non-goals

- No live tree-sitter parsing from the mention picker. Mentions read only from the prebuilt graph index.
- No DuckDB, Arrow/Parquet, or Onager dependency for this feature.
- No query-expression mentions in v1, such as `@callers(GraphNode)`. Graph traversal stays in MCP tools.
- No full-file dump for symbol mentions by default. Symbol mentions expand to the exact recorded symbol range.
- No new input syntax beyond existing `@` mention selection.

## 3. User Experience

The user types `@` and sees the existing mention picker augmented with code entities:

```text
@GraphEngine                     symbol:struct  crates/spur-pm/src/graph_engine/mod.rs:120-210
@GraphSnapshot                   symbol:struct  crates/spur-pm/src/graph_engine/snapshot.rs:18-96
@crates/spur-pm/src/graph_engine/mod.rs file          Rust
```

Selecting a file inserts a normal-looking atom:

```text
@crates/spur-pm/src/graph_engine/mod.rs
```

Selecting a symbol inserts a concise atom:

```text
@GraphEngine
```

The atom remains protected like existing file, worker, and issue mentions. Users can move across it and delete it atomically.

## 4. Mention Payload Contract

Accepted mentions continue to use the existing input-bar protected range model, but the URI identifies the graph entity.

File mention:

```text
display: @crates/spur-pm/src/graph_engine/mod.rs
uri: graph://file/<stable_file_id>
kind: file
file_path: crates/spur-pm/src/graph_engine/mod.rs
range: full file
graph_index_version: <run_id_or_hash>
```

Symbol mention:

```text
display: @GraphEngine
uri: graph://symbol/<stable_symbol_id>
kind: symbol
symbol_kind: struct
entity_name: GraphEngine
file_path: crates/spur-pm/src/graph_engine/mod.rs
line_range: 120-210
byte_range: 4120-8730
enclosing_scope: module graph_engine
graph_index_version: <run_id_or_hash>
```

The visible atom text is intentionally short. The hidden URI and source metadata carry the precise read target.

## 5. Source Model

Add a graph-backed mention source beside the existing file, worker, and issue sources.

```text
MentionRegistry
  FileMentionSource
  WorkerMentionSource
  IssueMentionSource
  CodeGraphMentionSource
```

`CodeGraphMentionSource` reads a local prebuilt graph-index artifact produced by the Graphify Phase 1 builder. It emits both file entries and symbol entries.

Minimum indexed fields:

| Field | File | Symbol | Purpose |
|---|---:|---:|---|
| stable id | yes | yes | Mention URI and MCP follow-up queries |
| display label | yes | yes | Picker row and atom text |
| search text | yes | yes | Fuzzy matching across path, symbol, and module |
| file path | yes | yes | One-read source target |
| line range | full | yes | Human-readable scope |
| byte range | full | yes | Exact slice extraction |
| kind | file | symbol kind | Row tag and expansion policy |
| graph index version | yes | yes | Staleness diagnostics |

## 6. Ranking

The picker should prefer exact and useful matches without hiding existing mention sources.

Recommended order for typed queries:

1. exact symbol name match;
2. exact file basename or path segment match;
3. fuzzy symbol match;
4. fuzzy path match;
5. existing workers and issues keep their current boosts where applicable.

Empty `@` should avoid flooding the picker with every symbol. Show a small mixed set:

- recently touched files or symbols if available;
- top-level files/directories from existing file source;
- worker entries in brain sessions, preserving current behavior.

## 7. Submit Expansion

On submit, SPUR should keep the user-visible text intact and attach structured mention metadata to the prompt assembly path. The prompt expansion should be compact and deterministic.

Symbol expansion:

```text
MENTION @GraphEngine
kind: symbol:struct
id: graph://symbol/<stable_symbol_id>
file: crates/spur-pm/src/graph_engine/mod.rs
lines: 120-210
graph_index_version: <run_id_or_hash>
source:
<exact source slice for the recorded symbol range>
```

File expansion:

```text
MENTION @crates/spur-pm/src/graph_engine/mod.rs
kind: file
id: graph://file/<stable_file_id>
file: crates/spur-pm/src/graph_engine/mod.rs
lines: full
```

Default symbol expansion is the exact symbol body/range only. Surrounding imports, callers, callees, and broader file context are not included by default; agents should request those through MCP tools when needed.

## 8. MCP Boundary

Mentions are anchors, not graph answers. They should make the first read exact. Deeper topology remains explicit tool use:

```text
get_callers("graph://symbol/<stable_symbol_id>")
get_callees("graph://symbol/<stable_symbol_id>")
get_subgraph("graph://file/<stable_file_id>", radius=1)
find_shortest_path(source_id, target_id)
rank_review_risk(changed_paths)
```

This keeps the TUI picker fast and predictable while giving brain and worker agents a stable bridge into graph-aware workflows.

## 9. Staleness And Failure Handling

If the graph index is missing:

- keep existing mention sources working;
- do not fall back to live parsing;
- optionally show no code-graph rows.

If the graph index is stale:

- still allow selection if the file exists and the recorded byte range validates against current content;
- mark the expansion with a staleness note;
- if the range no longer validates, degrade to a file mention for the same path and include a warning.

If a symbol name is ambiguous:

- show disambiguating row metadata: kind, file path, line range;
- accepted URI always points to one stable id.

## 10. Implementation Sketch

### TUI mention layer

- Extend `MentionKind` with a code entity variant or add a `Code`/`Symbol` kind while preserving existing file behavior.
- Add `CodeGraphMentionSource`.
- Extend `MentionEntry` metadata or add a side payload lookup keyed by URI so symbol range, symbol kind, and graph version survive selection.
- Keep `insert_atom` behavior unchanged: visible text, URI, and display name are enough for protected editing.

### Prompt assembly

- Resolve `graph://file/...` and `graph://symbol/...` atoms during submit.
- Read the exact byte range for symbol mentions.
- Include path, line range, kind, graph index version, and source slice.
- Enforce size limits per mention and per prompt.

### Graph index producer

- The Graphify Phase 1 builder owns the graph index schema.
- The TUI consumes the index read-only.
- The index must include enough fields for mention rows and expansion without reparsing.

## 11. Tests

Required tests:

- mention registry returns file and symbol entries from a fixture graph index;
- symbol rows include disambiguating kind, path, and line range;
- accepted symbol mention inserts an atomic protected range;
- prompt assembly expands `graph://symbol/...` into the exact fixture source range;
- missing graph index leaves existing file/worker/issue mentions unaffected;
- stale range validation degrades to file mention with warning;
- ambiguous symbols remain distinct by stable URI.

## 12. Acceptance Criteria

- Typing `@` can select both files and symbols from the prebuilt graph index.
- Accepted symbol mentions carry correct entity name, file path, and begin/end scope.
- Agents receive one precise read target for each symbol mention.
- Agents can use graph MCP tools for additional topology instead of receiving expanded neighborhoods by default.
- Existing file, worker, issue, and protected-atom behavior remains compatible.
