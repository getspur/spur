# Tree-sitter Code Mentions Design

**Status:** design approved (rev 2)
**Date:** 2026-05-12 (rev 2); 2026-05-11 (rev 1)
**Owner:** Kevin Truong
**Related decisions:**
- `docs/spur/graphify-rust-duckdb-onager-alternative.md`
- `bd-2hh` Graphify operational graph architecture decision
- `749ad09e docs(spur): bd-3bm define graph agent queries`

**Revision history:**
- rev 2 (2026-05-12): incorporated dual review (Gemini + Codex, first-principles + double-loop).
  Reframed §4 contract (stable URI + file path + validation policy authoritative; other fields are
  hints or display metadata). Revised §7 default to include a context header and explicit MCP
  affordance. Operationalized §9 validation predicate. Committed §10 to side-payload lookup keyed
  by URI. Added §6 ranking tests and §11 edge cases. Hybrid live-parsing question deferred —
  tracked as open question in §13.

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

Accepted mentions continue to use the existing input-bar protected range model. The contract is split
into three layers so that volatile fields cannot silently corrupt prompt assembly.

**Authoritative fields (the contract).** Prompt assembly MUST resolve every mention from these:

```text
display       — short atom text the user sees
uri           — graph://file/<stable_file_id> or graph://symbol/<stable_symbol_id>
kind          — file | symbol
file_path     — repo-relative path
validation    — the predicate prompt assembly must run before expansion (see §9)
```

**Extraction hints (advisory, validated, may be discarded).** These accelerate the common case but
are NEVER the source of truth. If validation fails, the hints are discarded and the mention follows
the degradation path in §9:

```text
line_range    — recorded line span (symbol only)
byte_range    — recorded byte span (symbol only)
symbol_kind   — struct | fn | trait | enum | … (symbol only)
entity_name   — recorded symbol identifier (symbol only)
```

**Display metadata (row rendering only, never read by prompt assembly).**

```text
enclosing_scope    — e.g. "module graph_engine"; non-authoritative, may be wrong, used only for
                     row disambiguation in the picker
graph_index_version — the run_id/hash of the index this row came from; used by §9 validation and
                     for staleness diagnostics; NOT a cache key for prompt assembly
```

File mention example:

```text
display:    @crates/spur-pm/src/graph_engine/mod.rs
uri:        graph://file/<stable_file_id>
kind:       file
file_path:  crates/spur-pm/src/graph_engine/mod.rs
validation: { kind: file_exists, path: <file_path> }
```

Symbol mention example:

```text
display:    @GraphEngine
uri:        graph://symbol/<stable_symbol_id>
kind:       symbol
file_path:  crates/spur-pm/src/graph_engine/mod.rs
validation: { kind: symbol_range, path, line_range, byte_range, entity_name, anchor_hash }
# extraction hints
line_range:  120-210
byte_range:  4120-8730
symbol_kind: struct
entity_name: GraphEngine
# display metadata (informational)
enclosing_scope:     module graph_engine
graph_index_version: <run_id_or_hash>
```

The visible atom text is intentionally short. The hidden contract carries the precise read target;
hints accelerate the common case; display metadata renders rows.

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

On submit, SPUR should keep the user-visible text intact and attach structured mention metadata to
the prompt assembly path. The prompt expansion should be compact and deterministic.

**Symbol expansion default — body plus context header.** Reviewer feedback (Gemini + Codex)
established that an isolated symbol body systematically starves the agent of context required to
reason about types, trait bounds, and module-level state. The default expansion therefore includes
a small, bounded *context header* alongside the exact symbol slice:

```text
MENTION @GraphEngine
kind:    symbol:struct
id:      graph://symbol/<stable_symbol_id>
file:    crates/spur-pm/src/graph_engine/mod.rs
lines:   120-210
graph_index_version: <run_id_or_hash>

context_header:
<file-level use/import block>
<module attributes and module-level constants relevant to the symbol>
<enclosing impl or trait signature line if the symbol is nested>

source:
<exact source slice for the recorded symbol range>

topology_available_via_mcp:
- get_callers("graph://symbol/<stable_symbol_id>")
- get_callees("graph://symbol/<stable_symbol_id>")
- get_subgraph("graph://file/<stable_file_id>", radius=1)
```

The `topology_available_via_mcp` block is an explicit affordance: it tells the agent that broader
topology is one tool call away. Without this hint, the MCP boundary in §8 becomes a hidden tax.

The context header is bounded by a per-mention size limit (see §10). If the header would exceed the
limit, it is truncated end-first with a `# … context truncated` marker; the symbol body itself is
never truncated — instead, prompt assembly fails the mention and follows the degradation path in §9.

File expansion:

```text
MENTION @crates/spur-pm/src/graph_engine/mod.rs
kind: file
id:   graph://file/<stable_file_id>
file: crates/spur-pm/src/graph_engine/mod.rs
lines: full
```

What is *not* included by default: callers, callees, sibling symbols in the same file, full file
body for symbol mentions. Agents should request those through MCP tools as needed.

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

### 9.1 Validation predicates

The `validation` field of every mention (§4) names the predicate prompt assembly must run before
expansion. The predicate is the source of truth; the extraction hints are advisory.

**`file_exists`** (file mentions):
- pass iff `file_path` resolves to a regular file in the worktree;
- on fail → "file missing" degradation (see §9.4).

**`symbol_range`** (symbol mentions): pass iff ALL of the following hold:
1. `file_path` resolves to a regular file in the worktree;
2. `byte_range` falls within the file's current byte length;
3. `byte_range` endpoints fall on UTF-8 character boundaries;
4. the slice at `byte_range` contains the recorded `entity_name` as a whole-word match;
5. `anchor_hash` (a stable hash of the first and last non-whitespace lines of the recorded slice,
   computed at index time) matches the same hash recomputed from current content.

If any of (1)–(5) fails, the predicate fails and the mention follows degradation (§9.4).

### 9.2 Missing graph index

- keep existing file, worker, and issue mention sources working;
- do NOT fall back to live parsing (deferred — see §13);
- show no code-graph rows in the picker;
- existing `@path/...` file mentions remain available.

### 9.3 Stale graph index (version mismatch)

If `graph_index_version` differs from the current expected version (e.g., the producer has run
since), the picker still allows selection, but every accepted mention's `validation` predicate
runs at submit time. A stale version does not by itself reject a mention — only a failed predicate
does.

### 9.4 Degradation path (predicate failure)

When `symbol_range` validation fails for a symbol mention, prompt assembly MUST:

1. emit a structured warning into the prompt, naming the user-intended symbol and why it failed:
   ```text
   MENTION_WARNING @GraphEngine
   intended_uri:   graph://symbol/<stable_symbol_id>
   failure_reason: anchor_hash_mismatch | range_out_of_bounds | utf8_boundary | name_not_found | file_missing
   replaced_with:  file_mention | dropped
   ```
2. if the file still exists, replace the symbol expansion with a `file` expansion for the same
   `file_path` so the agent retains some context;
3. if the file is missing, drop the mention entirely and emit the warning only;
4. preserve the user-visible atom text and the original `intended_uri` so the agent can decide
   whether to re-query via MCP.

The agent is never silently handed a different read target than the user asked for.

### 9.5 Ambiguous symbol names

- the picker shows disambiguating row metadata: `symbol_kind`, `file_path`, `line_range`,
  `enclosing_scope`;
- accepted URI always points to one `stable_symbol_id`;
- multiple symbols sharing a name appear as separate rows, never collapsed.

### 9.6 Ghost nodes (deleted symbols in stale index)

If the picker shows a row whose `file_path` no longer exists in the worktree, the row remains
selectable (the user may intend to recreate it). At submit time the `file_exists` clause of the
predicate fails, and §9.4 applies.

## 10. Implementation Sketch

### TUI mention layer

- Extend `MentionKind` with a `CodeFile` and `CodeSymbol` variant (or one `Code` variant tagged by
  `kind` payload). Preserve existing file behavior.
- Add `CodeGraphMentionSource` next to the existing `FileMentionSource`,
  `WorkerMentionSource`, `IssueMentionSource`.
- The atom embedded in the input bar carries ONLY the minimum: `display`, `uri`, `kind`,
  `file_path`. All other fields — hints, display metadata, and the `validation` predicate spec —
  live in a side payload keyed by `uri` and owned by `MentionRegistry`. Embedding only the minimum
  keeps protected-atom serialization small and lets the registry refresh side data without
  rewriting atoms.
- `insert_atom` behavior is unchanged.

### Prompt assembly

- Resolve `graph://file/...` and `graph://symbol/...` atoms during submit by looking up the side
  payload by `uri`.
- Run the `validation` predicate (§9.1). On pass, expand per §7. On fail, follow §9.4.
- Compose the prompt block: context header (symbol only), source slice, MCP affordance hint.
- Enforce size limits:
  - **per-mention cap:** 8 KB total. Context header is bounded to 1.5 KB; if it would exceed,
    truncate end-first with `# … context truncated`. The symbol body itself is never truncated —
    a body that exceeds (8 KB − header) fails the mention via §9.4.
  - **per-prompt cap:** sum of all mention expansions bounded to 32 KB. Excess mentions are
    replaced with a single-line stub `MENTION_OMITTED <uri> (per-prompt cap)` so the agent knows
    something was elided.
  - UTF-8 boundary safety: all truncations land on character boundaries.

### Graph index producer

- The Graphify Phase 1 builder owns the graph index schema.
- The TUI consumes the index read-only.
- The producer MUST emit, per symbol: `stable_symbol_id`, `file_path`, `byte_range`, `line_range`,
  `entity_name`, `symbol_kind`, `anchor_hash` (see §9.1.5), and `enclosing_scope` (display only).
- Per file: `stable_file_id`, `file_path`.
- The index header MUST include `graph_index_version` (run id or content hash of the producer
  inputs).

## 11. Tests

### 11.1 Registry and picker

- mention registry returns file and symbol entries from a fixture graph index;
- symbol rows include disambiguating `symbol_kind`, `file_path`, `line_range`, `enclosing_scope`;
- accepted symbol mention inserts an atomic protected range carrying only `display | uri | kind |
  file_path`, with hints/display metadata resolvable via side-payload lookup;
- atom survives cursor movement, partial selection, and undo/redo without losing its URI;
- protected-atom delete removes the atom and its side payload together (no orphan payload).

### 11.2 Ranking (§6)

- exact symbol name match outranks exact file basename match for an identical typed query;
- exact file basename match outranks fuzzy symbol match;
- fuzzy symbol match outranks fuzzy path match;
- existing worker and issue boosts are preserved in brain sessions; code-graph rows do not
  starve worker/issue rows for typed queries that match a worker or issue;
- empty `@` shows a small mixed set, never the full symbol table;
- collision case: a file `config.rs` and a struct `Config` for the keystrokes `Co` — both must be
  visible and unambiguously labeled in the picker.

### 11.3 Validation and degradation (§9)

- prompt assembly expands `graph://symbol/...` into the exact fixture source range when the
  predicate passes;
- `symbol_range` predicate fails when `byte_range` is out of bounds → degradation emits a
  `MENTION_WARNING` and replaces with a file mention;
- predicate fails when the slice does not contain `entity_name` (range points to valid bytes but
  the wrong symbol) → degradation;
- predicate fails when `anchor_hash` mismatches (symbol body has shifted) → degradation;
- predicate fails on UTF-8 boundary violation → degradation;
- predicate fails when `file_path` is deleted → mention is dropped, warning emitted;
- predicate fails when the file was renamed and `file_path` no longer exists → mention is dropped
  (rename detection is out of scope for v1);
- missing graph index leaves existing file/worker/issue mentions unaffected; no code-graph rows
  appear;
- ambiguous symbols remain distinct by `stable_symbol_id`; each has its own row.

### 11.4 Expansion (§7)

- symbol expansion includes the context header (file-level use/import block + module attrs +
  enclosing impl/trait signature when applicable);
- symbol expansion includes the `topology_available_via_mcp` affordance block;
- context header truncates end-first when it exceeds the 1.5 KB bound, with the
  `# … context truncated` marker on a character boundary;
- symbol body exceeding (8 KB − header) fails the mention via §9.4 rather than being truncated;
- per-prompt cap: when the sum of expansions exceeds 32 KB, the excess mentions are replaced with
  a `MENTION_OMITTED` stub in insertion order.

### 11.5 Malformed / adversarial artifacts

- malformed graph index file (truncated, invalid JSON) is rejected with a single diagnostic; the
  feature degrades to "no code-graph rows" rather than crashing the picker;
- duplicate `stable_symbol_id` entries are deduplicated by the first occurrence; a diagnostic is
  emitted;
- byte ranges with reversed endpoints (`end < start`) fail validation deterministically.

## 12. Acceptance Criteria

- Typing `@` can select both files and symbols from the prebuilt graph index.
- Accepted symbol mentions carry the authoritative contract fields (§4): `display`, `uri`, `kind`,
  `file_path`, and the `validation` predicate. Other fields are hints (validated) or display
  metadata (informational).
- The `validation` predicate (§9.1) runs at submit time for every code-graph mention; failure
  follows the degradation path in §9.4, never silent substitution.
- Default symbol expansion (§7) includes the exact symbol body, a bounded context header, and an
  explicit `topology_available_via_mcp` affordance — the agent is told topology is one tool call
  away.
- Agents can use graph MCP tools for additional topology instead of receiving expanded
  neighborhoods by default.
- Per-mention and per-prompt size limits (§10) are enforced; truncation lands on UTF-8 boundaries;
  symbol bodies are never silently truncated.
- Existing file, worker, issue, and protected-atom behavior remains compatible.
- Picker ranking (§6) is deterministic and tested; file-vs-symbol name collisions remain
  unambiguous in the picker.

## 13. Open Questions

1. **Hybrid live parsing for dirty files.** Gemini's review argued that strictly disallowing live
   tree-sitter parsing (§2) throws away tree-sitter's defining advantage and exposes users to
   staleness on the very files they are actively editing. A hybrid model — Graphify index for the
   global repo, live tree-sitter for the active/dirty file set — would eliminate that staleness
   class. Deferred for v1 to keep scope tight, but should be revisited once §9 staleness
   diagnostics are observed in practice.
2. **`anchor_hash` collision tolerance.** §9.1.5 defines `anchor_hash` over the first and last
   non-whitespace lines of the recorded slice. This survives most edits inside the body but fails
   on edits to the first/last lines (e.g., changing a function signature). Should the predicate
   be relaxed to "name match + range size within tolerance" for symbols whose signature line has
   changed, or is hard-fail-then-degrade the right behavior?
3. **Topology affordance budget.** §7 includes a `topology_available_via_mcp` block in every
   symbol expansion. Should this be suppressed after the agent has demonstrably called an MCP
   topology tool, to avoid repeating the affordance on every subsequent mention in the same
   session?
