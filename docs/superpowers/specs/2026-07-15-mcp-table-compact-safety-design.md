# MCP Table/Compact Format and Analyst Query Safety Design

Date: 2026-07-15

Issue: `bd-1qd2l`

## Context

A three-layer review of `crates/spur-graph/src/mcp/` and
`crates/spur-analyst/src/mcp/tools/` found five related defects:

1. The analyst `query` tool rejects writes by inspecting only the first lexical
   token, so leading comments and multiple statements bypass the guard.
2. The deprecated `knowledge_context_pack` tool advertises v2 behavior but
   still parses and executes the v1 path.
3. `response_format: "table"` is not preserved across ambiguous selectors or
   incompatible format combinations.
4. `code_symbol_search` advertises table format without producing a table.
5. `code_callers` defaults `include_unresolved` to false, conflicting with the
   graph-navigation contract for impact analysis.

The table encoding is materially useful: on a representative five-row caller
response it reduced serialized output from 2,780 bytes (`full`) and 2,385 bytes
(`compact`) to 1,925 bytes. The implementation should make that encoding
reliable before changing broad defaults.

## Goals

- Enforce that analyst SQL is one side-effect-free, read-only statement.
- Make both knowledge-pack public names execute the same v2 implementation.
- Make every advertised table response self-identifying and structurally
  table-shaped on every successful result path.
- Give `code_symbol_search` a real interned table representation.
- Default unresolved caller rows on while retaining the callee default off.
- Preserve existing object/full behavior when callers omit `response_format`
  during this compatibility phase.

## Non-goals

- Changing every MCP tool's omitted-format default in this patch.
- Reformatting the entire knowledge-context v2 envelope as a table.
- Redesigning graph identifiers, traversal resolution, or table column names
  unrelated to the findings.
- Broad refactors outside the graph and analyst MCP surfaces required for
  schemas, dispatch, and tests.

## Approaches Considered

### 1. Minimal branch-by-branch patches

Add special cases at each failing return site and extend the SQL token
blocklist. This has the smallest diff, but it preserves duplicated response
logic and leaves the SQL guard vulnerable to new statement forms.

### 2. Centralized compatibility-first policy (selected)

Introduce format-aware response helpers for candidate and ambiguity results,
route aliases through one v2 handler, and replace the SQL blocklist with a
single-statement read-only validator. Keep omitted graph format as `full` for
this patch. This closes the defects without forcing existing clients to adopt
a new decoder immediately.

### 3. Versioned default flip

Make table the default for every collection tool and compact/source the default
for object tools immediately, behind a new API version. This is the cleanest
eventual contract, but it expands scope into catalog versioning and migration.
It should follow after the table contract is stable and measured.

## Design

### Analyst SQL validation

`query` must validate the complete SQL input before opening or using the pooled
connection. Validation has two independent requirements:

1. Exactly one executable statement is present after comments and whitespace.
2. The statement is in an allowlist of side-effect-free query forms.

The implementation should prefer an existing DuckDB/parser facility already
available in the workspace. If no suitable parser API exists, add a small
scanner that understands SQL strings, quoted identifiers, line comments, and
block comments well enough to split statements safely, then apply a strict
allowlist. A blocklist alone is not acceptable.

Allowed top-level forms are read-oriented operations required by the existing
tool contract, such as `SELECT`, read-only `WITH`, `SHOW`, `DESCRIBE`, and
read-only `EXPLAIN`. `WITH` and `EXPLAIN` must not become wrappers that admit a
write-capable inner statement. Statement families capable of mutating state,
loading extensions, attaching databases, changing settings, or writing files
must be rejected before DuckDB execution, including `COPY`, `EXPORT`,
`INSTALL`, `LOAD`, `ATTACH`, and `PRAGMA`.

Validation failures remain `McpHandlerError::InvalidParams`. DuckDB preparation
and execution failures retain the existing structured query-error response.

### Knowledge-context alias unification

Both `knowledge_context_pack` and `knowledge_context_pack_2` will parse
`KnowledgeContextPackV2Request` and call
`service::knowledge_context_pack_2`. The deprecated name remains advertised as
an alias, but it no longer has a distinct response implementation. V1-only
service code may remain temporarily if internal tests or non-MCP callers still
use it; the public MCP dispatch must not route through it.

Both public names must therefore share:

- graph-reasoning options;
- overlay-aware staleness behavior;
- risk, community, temporal, graph-path, and caveat sections;
- validation and unavailable-response behavior.

### Graph response-format contract

Add one format-aware candidate response path used by ambiguity and symbol
search. For `ResponseFormat::Table`, candidate collections use `candidate_table`
and `TableFileInterner`, include `response_format: "table"`, and include the
top-level `files` array. For `Full` and `Compact`, candidates retain their
current object representation; compact continues to omit healthy graph
metadata.

The selected shape must survive every successful branch:

- direct indexed response;
- worktree overlay/rebuild response;
- ambiguous selector response;
- temporal/current-worktree resolution where supported.

`code_subgraph` accepts table shape only with `format: "json"`. Combining
`response_format: "table"` with `format: "mermaid"` returns
`InvalidParams` instead of silently ignoring the table request.

`code_read_symbol` continues to advertise `full|compact|source`; internal
parsing must not silently accept an unsupported table request. Tools that do
not advertise table must reject it or use a parser constrained to their
advertised variants.

### Caller versus callee unresolved defaults

Split traversal parsing by direction or pass an explicit default:

- `code_callers`: `include_unresolved` defaults to `true`.
- `code_callees`: `include_unresolved` defaults to `false`.
- `code_subgraph`: remains `false` unless explicitly requested.

Schemas, tool descriptions, structured responses, and tests must agree. The
response continues to echo the effective boolean and always reports summary
counts and an unresolved sample.

### Compatibility and migration

Omitting `response_format` continues to select `full` for existing graph tools
in this patch. The table and compact modes become reliable opt-ins. A later
versioned change may select defaults by result shape:

| Result shape | Future preferred default |
|---|---|
| Repeated records | `table` |
| Single metadata object | `compact` |
| Source body | `source` |
| Diagnostic identity metadata | explicit `full` |

The analyst SQL `query` response already has a column/row table shape and does
not need a second nested table encoding in this patch.

## Testing Strategy

Implementation follows failing-test-first commits.

### Analyst query tests

- Reject direct write statements already covered today.
- Reject a prohibited statement after a harmless leading `SELECT`.
- Reject prohibited statements after line and block comments.
- Reject multiple read statements as well as mixed read/write statements.
- Reject write-capable `WITH`/`EXPLAIN` wrappers.
- Reject `COPY`, `EXPORT`, `INSTALL`, `LOAD`, `ATTACH`, setting changes, and
  mutating `PRAGMA`/all PRAGMA if the validator cannot prove read-only behavior.
- Continue accepting supported `SELECT`, read-only `WITH`, `SHOW`,
  `DESCRIBE`, and read-only `EXPLAIN` forms.

### Knowledge-context tests

- Dispatch both public names with identical v2 arguments and compare their
  normalized response shapes.
- Assert the deprecated name honors graph-reasoning options.
- Assert both names use overlay-aware staleness behavior.

### Graph format tests

- `code_symbol_search` table response has `cols`, `rows`, file interning, and
  is smaller than the full response on a repeated-file fixture.
- Ambiguous callers, callees, subgraph, and read/resolve surfaces either return
  the requested supported shape or reject unsupported format values.
- Mermaid plus table is rejected.
- Overlay/rebuild table paths preserve the requested shape.
- Full and compact snapshots remain compatible.

### Traversal-default tests

- Omitting `include_unresolved` returns unresolved caller records and echoes
  `true` for callers.
- Omitting it still filters unresolved callee records and echoes `false`.
- Explicit booleans override both defaults.

## Verification

Run through `scripts/spur-cargo`:

```text
scripts/spur-cargo fmt --all
scripts/spur-cargo test -p spur-analyst --test pack
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo test -p spur-core --test code_graph_e2e
scripts/spur-cargo test -p spur-core --test worker_server_dispatch
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-analyst -p spur-graph -p spur-core -- -D warnings
```

## Rollout

Land the safety and contract fixes without changing omitted graph format.
Document table as preferred for collection-heavy agent calls and compact/source
for single-object calls. Consider a later versioned default change only after
all public tool surfaces have format-complete tests.
