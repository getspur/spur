# `notebook.edit_cell` — partial cell edits via string replacement

**Date:** 2026-06-10
**Status:** approved
**Crate:** `crates/spur-notebook` (MCP tool layer only — no frontend or daemon-store changes)

## Problem

The only way for an agent to modify a notebook cell is `notebook.write_cell`
(`crates/spur-notebook/src/mcp/tools/write_cell.rs`), which requires the agent to
regenerate the **entire** cell source to change one line. For large cells (Spur App
frontend cells, datasource setup, long analysis blocks) this wastes output tokens, adds
latency, and risks the model truncating or mangling untouched code while retyping it.

## Industry grounding (researched 2026-06-10)

- Anthropic tested several edit strategies for its SWE-bench agent and found
  **string replacement (`str_replace`)** the most reliable; it is the format behind
  Claude Code's `Edit`, SWE-agent's editor, Gemini CLI's `replace`, and Cline.
  Claude-family models are heavily post-trained on it.
- Strict git/unified diffs are the format the industry tried and abandoned: models get
  line numbers wrong constantly. Aider's udiff and OpenAI's V4A both had to strip line
  numbers and apply hunks as context-anchored search/replace with fuzzy matching.
- Cursor's data: full rewrite beats diffs for content under ~400 lines. Most notebook
  cells are short — so `write_cell` stays the right tool for short cells and new
  content; `edit_cell` targets surgical changes in long cells.

Design follows the Anthropic `str_replace` contract: exactly-one-match enforcement with
descriptive errors that drive a retry loop.

## Design — Option A: compose existing bridge methods in the Rust tool layer

New tool `notebook.edit_cell` implemented entirely in
`crates/spur-notebook/src/mcp/tools/edit_cell.rs`. Internally it performs:

1. bridge `notebook.read_cell { id }` → current `source` + `version`
2. apply string-replacement edits to `source` in Rust
3. bridge `notebook.write_cell { id, source, expected_version: <read version>, last_edited_by: "brain" }`

Zero changes to `jute-notebook/src/agent/handlers.ts` or
`src-tauri/src/notebook_store.rs`. The existing optimistic-concurrency check in the
frontend handler (`writeCell` rejects on version mismatch with code `stale_version`)
makes the read-modify-write race-safe.

### Tool schema

```json
{
  "type": "object",
  "required": ["id", "edits"],
  "properties": {
    "id": { "type": "string", "minLength": 1 },
    "edits": {
      "type": "array",
      "minItems": 1,
      "items": {
        "type": "object",
        "required": ["old_string", "new_string"],
        "properties": {
          "old_string": { "type": "string", "minLength": 1 },
          "new_string": { "type": "string" },
          "replace_all": { "type": "boolean", "default": false }
        },
        "additionalProperties": false
      }
    },
    "expected_version": { "type": "integer", "minimum": 1 }
  },
  "additionalProperties": false
}
```

Tool description (teaches the format — descriptions are load-bearing per Anthropic's
tool-design research):

> Apply targeted string-replacement edits to one cell without rewriting its full
> source. Each `old_string` must match the current cell source exactly once (include
> surrounding context to disambiguate) unless `replace_all` is true. Edits apply in
> order. Prefer this over `notebook.write_cell` for small changes to large cells;
> prefer `notebook.write_cell` for short cells or full rewrites.

Also extend the `notebook.write_cell` description with the converse hint:
`"… For small changes to large cells prefer notebook.edit_cell."`

### Semantics

- **Validation (before any bridge call):** `id` non-empty; `edits` non-empty; each
  `old_string` non-empty; `old_string != new_string` per edit. Violations →
  `McpError::invalid_params` naming the offending edit index.
- **Version pinning:** if `expected_version` is provided and the version returned by
  `read_cell` differs, fail with `invalid_params` reporting both versions (message
  includes `stale_version`) **without writing**.
- **Edit application (sequential, in order, against the evolving source):** for each
  edit, count occurrences of `old_string` (non-overlapping, exact byte match):
  - `0` matches → error: `notebook.edit_cell edit N: old_string not found in cell <id>`
  - `>1` matches and `replace_all` is false → error:
    `notebook.edit_cell edit N: old_string matched K times in cell <id>; include more surrounding context to make it unique, or set replace_all`
  - otherwise replace (first/only occurrence, or all when `replace_all`).
  - Any edit error aborts the whole call before the write — edits are atomic as a set.
- **No-change short-circuit:** if the final source equals the original, skip the write
  and return the current version with `changed: false`.
- **Stale-write retry:** if the `write_cell` bridge call fails with
  `BridgeError::Handler { code: "stale_version", .. }` **and** the caller did not pin
  `expected_version`, re-run the read→apply→write loop **once**. If it fails again (or
  the caller pinned a version), propagate via `into_mcp_error()`.

### Result shape

```json
{ "version": 8, "replacements": 3, "changed": true, "snippet": "…" }
```

- `version`: version returned by `write_cell` (or current version when `changed: false`).
- `replacements`: total occurrences replaced across all edits.
- `snippet`: up to ~10 lines of the new source centered on the location of the **last**
  applied edit — lets the agent verify without a follow-up `read_cell`. Omit (or empty
  string) when `changed: false`.

### Registration

- Add `pub mod edit_cell;` to `crates/spur-notebook/src/mcp/tools/mod.rs` (alphabetical
  with the existing module list) and `edit_cell::tool()` to `tools()` next to
  `write_cell::tool()`.
- Wire dispatch in the same match that routes `write_cell::call` (follow how
  `write_cell` is dispatched from the MCP server — find its `call` site and mirror it).

## Implementation notes

- Follow `write_cell.rs` as the structural template: `METHOD` const, params struct with
  `serde::Deserialize`, `tool()` with `rmcp_object` schema, `pub async fn call(deps:
  &ServerDeps, arguments: Value)`.
- `LAST_EDITED_BY = "brain"` (same constant value as `write_cell.rs`).
- Use `super::BRIDGE_TIMEOUT` for both bridge calls.
- The pure edit-application function should be a free function
  (`fn apply_edits(source: &str, edits: &[CellEdit]) -> Result<AppliedEdits, EditError>`)
  so it is unit-testable without a bridge.
- Occurrence counting: `str::matches(old_string).count()`; replacement:
  `str::replacen(old, new, 1)` / `str::replace` for `replace_all`.

## Tests (TDD — `test(...)` commit first, then `feat(...)`)

Unit tests in `edit_cell.rs` `#[cfg(test)]`, following the existing patterns:
`TestBridge` impls of `BridgeRequester` as in `notebook_push_source.rs` /
`notebook_dag_status.rs`, and a capturing variant as in `notebook_set_dag_metadata.rs`
(`CapturingBridge`) that serves `notebook.read_cell` with a fixed source/version and
records the `notebook.write_cell` payload.

1. **Happy path:** single edit → write payload contains spliced source,
   `expected_version` equals the version served by read, `last_edited_by == "brain"`;
   result has bumped `version`, `replacements: 1`, `changed: true`, non-empty snippet.
2. **Not found:** `old_string` absent → error names edit index and cell id; **no write
   request dispatched** (assert via capturing bridge).
3. **Ambiguous:** 2+ matches without `replace_all` → error includes match count; no write.
4. **replace_all:** replaces every occurrence; `replacements` reflects the count.
5. **Sequential edits:** edit 2 matches text produced by edit 1 (order matters).
6. **Pinned version mismatch:** `expected_version: 3` while read returns version 5 →
   stale error before any write.
7. **No-change short-circuit:** edits produce identical source → no write,
   `changed: false`.
8. **Stale-write retry:** TestBridge returns `BridgeError::Handler { code:
   "stale_version" }` on the first write, success on the second; with unpinned
   `expected_version` the tool retries once and succeeds; with pinned version it
   propagates immediately.
9. **Param validation:** empty `edits`, empty `old_string`, `old_string == new_string`
   → `invalid_params`, no bridge traffic.
10. **Registry:** extend the `tools_include_direct_notebook_file_tools` test in
    `mod.rs` to assert `notebook.edit_cell` is present.

## Build & verification

- `scripts/spur-cargo test -p spur-notebook` (remote-default; a red remote test is a
  real failure)
- `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`
- `scripts/spur-cargo fmt --all`

## Commits

1. `docs(specs): notebook.edit_cell partial-edit spec`
2. `test(spur-notebook): edit_cell str-replace semantics and bridge dispatch`
3. `feat(spur-notebook): add notebook.edit_cell partial-edit tool`

## Out of scope (explicitly)

- V4A/udiff-style context-anchored diff input (revisit only if Codex-worker edit-format
  error telemetry justifies it)
- Native bridge method / `NotebookOp::EditCell` in the daemon store (Option B — only if
  Option A's retry loop proves noisy)
- Frontend (`handlers.ts`) and `notebook_store.rs` changes
