---
name: code-explore
description: "You MUST use this before any code exploration, navigation, or impact analysis — instead of Grep/Glob/Read-walking. Establishes the code_* MCP graph tools as the primary substrate for finding symbols, mapping call relationships, and bounding blast radius before edits."
role: both
---

# Code Explore — Graph-First Navigation

The SPUR graph artifact already knows every symbol, definition site, call edge, and reference in the worktree. Use the `code_*` MCP tools to query it. Text search is a fallback, not a starting point.

<HARD-GATE>
Before opening more than one file with Read, or running more than one Grep/Glob to locate code, you MUST attempt the relevant `code_*` tool. Grepping for a function name when `code_resolve` would return its definition is the anti-pattern this skill exists to prevent.
</HARD-GATE>

## Why graph-first

- **Precise:** edges are extracted by the language parser, not by string match. No false positives from comments, docstrings, or unrelated identifiers.
- **Cheap:** one call returns structured rows. Grep for a common name returns hundreds of lines you must then re-read.
- **Complete:** captures resolved callees AND unresolved call labels, so macro-bodied or HOF call sites are visible.
- **Cache-friendly:** every response carries `graph_content_hash` + `indexed_head_oid` so you can detect staleness instead of re-reading.

## The seven tools

| Tool | Use when |
|---|---|
| `code_resolve` | You have a name/qualified-name and want candidate definitions (no edges). Cheapest disambiguation step. |
| `code_symbol_info` | You already know the selector and just need file/line/kind metadata. |
| `code_file_symbols` | You want the symbol outline of one worktree-relative file. Replaces "Read the whole file to see what's in it." |
| `code_search` | Lexical fallback. Reach for it when `code_resolve`/`code_callees` returns empty (e.g. macro bodies), or when you only know a fragment. Supports `mode=exact|prefix|substring`, `symbol_kind` filter, `file`/`file_glob` scoping. |
| `code_callers` | Impact analysis: "what will break if I change X?" |
| `code_callees` | Behavior analysis: "what does X actually do?" Returns both resolved edges and unresolved labels. |
| `code_subgraph` | Bounded N-hop neighborhood. Use `format=mermaid` for human-readable maps, `edge_kinds=["calls"]` or `["references"]` to scope. `radius` is clamped to 3. |

## Selector grammar

All edge tools accept a single `selector` string. In order of specificity:

1. `graph://symbol/<hex-id>` — canonical, unambiguous (from any prior tool response's `uri`).
2. Bare `<hex-id>` — same effect.
3. File-qualified: `crates/foo/src/bar.rs::my_fn` — disambiguates same-named symbols.
4. Qualified name: `module::path::name`.
5. Bare name: `my_fn` — may be ambiguous; tools return candidates by default (`on_ambiguous=candidates`).

Prefer (1) once you have it. Carry the `uri` from response to response instead of re-resolving by name.

## Checklist — Standard exploration loop

You MUST complete these in order. Skip steps only when their output is already in context.

1. **Locate** — `code_resolve` (or `code_search` if name is partial) to get candidate `uri`s. Disambiguate before continuing.
2. **Outline** — if the target's file is unfamiliar, `code_file_symbols` to see the file's surface area.
3. **Inwards** — `code_callees` on the target to understand what it does.
4. **Outwards** — `code_callers` on the target to scope the blast radius of changes.
5. **Bound** — `code_subgraph radius=2 format=mermaid` when the 1-hop view doesn't answer the question. Stop at radius 3; if you need more, the question is too broad — decompose it.
6. **Read** — only now reach for `Read` on specific `file_path` + `line_range` returned by the graph. Never read whole files when the graph already gave you the range.

## Process Flow

```dot
digraph code_explore {
    "Need to navigate code" [shape=doublecircle];
    "Have a uri already?" [shape=diamond];
    "code_resolve / code_search" [shape=box];
    "Ambiguous?" [shape=diamond];
    "Pick candidate or refine selector" [shape=box];
    "Question type?" [shape=diamond];
    "code_callees" [shape=box];
    "code_callers" [shape=box];
    "code_subgraph radius=2" [shape=box];
    "code_file_symbols" [shape=box];
    "Read narrow line_range" [shape=box];
    "Answer assembled" [shape=doublecircle];

    "Need to navigate code" -> "Have a uri already?";
    "Have a uri already?" -> "Question type?" [label="yes"];
    "Have a uri already?" -> "code_resolve / code_search" [label="no"];
    "code_resolve / code_search" -> "Ambiguous?";
    "Ambiguous?" -> "Pick candidate or refine selector" [label="yes"];
    "Pick candidate or refine selector" -> "Question type?";
    "Ambiguous?" -> "Question type?" [label="no"];
    "Question type?" -> "code_callees" [label="what does it do"];
    "Question type?" -> "code_callers" [label="what breaks if I change it"];
    "Question type?" -> "code_subgraph radius=2" [label="neighborhood / map"];
    "Question type?" -> "code_file_symbols" [label="what's in this file"];
    "code_callees" -> "Read narrow line_range";
    "code_callers" -> "Read narrow line_range";
    "code_subgraph radius=2" -> "Read narrow line_range";
    "code_file_symbols" -> "Read narrow line_range";
    "Read narrow line_range" -> "Answer assembled";
}
```

## Red Flags — Stop and switch to code_*

| Thought | Reality |
|---|---|
| "Let me grep for the function name." | `code_resolve` returns the definition directly — no false positives. |
| "I'll read the file to find callers." | `code_callers` returns every caller's file+line in one call. |
| "Let me Read the whole file to see structure." | `code_file_symbols` gives the outline in one call. |
| "Grep returned 80 matches — let me filter." | `code_search` with `symbol_kind` + `file_glob` filters in-tool. |
| "I'll trace this by reading each callsite." | `code_subgraph radius=2 format=mermaid` produces the trace map. |
| "The macro hides the call so the graph won't help." | `code_search` is the documented fallback for opaque macro bodies. |

## Anti-patterns

- **Re-resolving by name across turns.** Once you have a `uri`, pass it. Names are slower and may collide.
- **`radius=3` as the default.** Start at 1, go to 2 only when 1-hop is insufficient. Bigger radii return more nodes than you can usefully read.
- **Ignoring `resolved: false` callees.** Unresolved labels are signal: dynamic dispatch, macro expansion, or HOF arg. Surface them in your analysis, don't drop them.
- **Trusting stale graph data silently.** Every response includes `indexed_head_oid` and `worktree_dirty`. If `worktree_dirty: true` and your question touches uncommitted code, say so before relying on the answer.
- **Treating `code_search` as primary.** It's a lexical fallback. If `code_resolve` would have worked, use it — search returns ranked guesses, resolve returns the definition.

## Key Principles

- **Graph before text.** Every code question gets a `code_*` attempt before Grep/Glob/Read-walking.
- **Carry the uri.** Resolve once, use the `graph://symbol/<id>` across the rest of the investigation.
- **Bound, don't enumerate.** Use `radius` and `edge_kinds` to shape the answer instead of trimming a huge result by hand.
- **Read narrowly.** When you finally Read, pass `offset`/`limit` matching the graph's `line_range`.
- **Surface staleness.** If `worktree_dirty` or `indexed_head_oid` lags `worktree_head_oid` and matters, flag it.
