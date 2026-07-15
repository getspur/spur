# MCP Table/Compact Safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the analyst SQL read-only bypasses and make the graph MCP table/compact contracts reliable, with table prioritized for repeated records and unresolved callers included by default.

**Architecture:** Keep the compatibility-first design in `docs/superpowers/specs/2026-07-15-mcp-table-compact-safety-design.md`: validate one complete SQL statement before connection use, route both knowledge-pack names through the v2 adapter, centralize graph candidate/table shaping, and pass direction-specific traversal defaults. Omitted graph `response_format` remains `full`; this patch hardens explicit `table`, `compact`, and `source` behavior without a versioned default flip.

**Tech Stack:** Rust 2021, `serde_json`, DuckDB through the existing `spur-analyst` connection layer, graph MCP handlers in `spur-graph`, `schemars`/RMCP worker schemas in `spur-core`, Tokio tests, and `scripts/spur-cargo` for every build/test/lint command.

---

## Working rules

- [ ] Read the approved design before editing:

  ```text
  docs/superpowers/specs/2026-07-15-mcp-table-compact-safety-design.md
  ```

- [ ] Use `code-explore` before new navigation, `spurpower-test-driven-development` for each defect, and `spurpower-verification-before-completion` before reporting success.
- [ ] Work only in the delegated worktree. Preserve unrelated changes and do not edit `.spur/config.toml`, package migration files, generated plans, or user assets.
- [ ] Use `scripts/spur-cargo`, never bare `cargo`.
- [ ] Commit each failing regression set before its implementation commit, as required by this repository's bug-fix cadence.

## Task 1: Specify the complete analyst SQL safety boundary

**Files:**

- Modify: `crates/spur-analyst/tests/pack/mcp_query.rs`
- Modify: `crates/spur-analyst/src/mcp/tools/query.rs` (unit tests only in this task)

- [ ] Add an integration regression named `query_rejects_commented_and_stacked_statements` that dispatches through `AnalystMcpModule` and expects `McpHandlerError::InvalidParams` containing `read-only` for each of these inputs:

  ```sql
  -- harmless prefix
  PRAGMA version

  /* harmless prefix */ LOAD 'json'

  SELECT 1; PRAGMA version

  SELECT 1; SELECT 2

  WITH seed AS (SELECT 1) DELETE FROM facts

  EXPLAIN INSERT INTO facts VALUES (1)
  ```

  Seed `facts` where needed, but assert rejection happens at the MCP validation boundary rather than as a structured DuckDB execution error.

- [ ] Add a `query_validation` unit-test module in `query.rs` with table-driven regressions for all prohibited statement families from the design: `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `ALTER`, `DROP`, `ATTACH`, `DETACH`, `COPY`, `EXPORT`, `INSTALL`, `LOAD`, `PRAGMA`, `CALL`, `SET`, transaction/control statements, and multiple executable statements.

- [ ] Add accepted-form tests for comments plus one `SELECT`, a semicolon inside a quoted string, read-only `WITH`, `SHOW`, `DESCRIBE`, and `EXPLAIN SELECT`. A trailing semicolon followed only by comments must still count as one statement.

- [ ] Run the new tests and confirm they fail against the current first-token guard:

  ```bash
  scripts/spur-cargo test -p spur-analyst --test pack mcp_query::query_rejects_commented_and_stacked_statements -- --exact
  scripts/spur-cargo test -p spur-analyst --lib query_validation
  ```

- [ ] Commit only the failing regressions:

  ```bash
  git add crates/spur-analyst/tests/pack/mcp_query.rs crates/spur-analyst/src/mcp/tools/query.rs
  git commit -m "test(spur-analyst): bd-1qd2l cover SQL guard bypasses"
  ```

## Task 2: Replace the first-token blocklist with one-statement validation

**Files:**

- Modify: `crates/spur-analyst/src/mcp/tools/query.rs`
- Reference only: `crates/spur-cli/src/commands/analyst.rs:291` (`SqlScanState`, `split_sql_statements`, and `statement_has_sql`)

- [ ] Replace `reject_write_statement`/`first_token` with `validate_read_only_query`, called immediately after `QueryRequest::parse` and before `analyst_db_path` or pooled-connection access.

- [ ] Adapt the existing CLI scanner pattern locally instead of introducing a cross-crate dependency. The scanner must:

  - distinguish normal text, single-quoted strings, double-quoted identifiers, line comments, and block comments;
  - ignore semicolons and keyword-like text inside quotes/comments;
  - split on normal-state semicolons;
  - discard whitespace/comment-only fragments;
  - reject unterminated quotes/comments as invalid parameters;
  - produce normalized normal-state identifier tokens for policy checks.

- [ ] Enforce exactly one executable statement. Return an `InvalidParams` message that states whether zero or multiple statements were found.

- [ ] Apply a strict read-oriented lead-token allowlist:

  ```rust
  const READ_ONLY_LEAD_TOKENS: &[&str] =
      &["SELECT", "WITH", "SHOW", "DESCRIBE", "DESC", "EXPLAIN"];
  ```

  Reject every other lead token. Also reject dangerous tokens anywhere in the executable statement so `WITH` and `EXPLAIN` cannot wrap writes or control operations. Include at least `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `ALTER`, `DROP`, `ATTACH`, `DETACH`, `COPY`, `EXPORT`, `IMPORT`, `INSTALL`, `LOAD`, `PRAGMA`, `CALL`, `SET`, `RESET`, `BEGIN`, `START`, `COMMIT`, `ROLLBACK`, `CHECKPOINT`, `VACUUM`, `GRANT`, and `REVOKE`.

- [ ] Keep validation conservative: quoted identifiers and string contents are not policy tokens; an unquoted dangerous keyword is rejected even when DuckDB would later report a syntax error. The read-only DuckDB connection remains defense in depth.

- [ ] Run the complete query surface:

  ```bash
  scripts/spur-cargo test -p spur-analyst --test pack mcp_query
  scripts/spur-cargo test -p spur-analyst --lib query_validation
  ```

- [ ] Commit the safety fix:

  ```bash
  git add crates/spur-analyst/src/mcp/tools/query.rs
  git commit -m "fix(spur-analyst): bd-1qd2l validate complete read-only SQL"
  ```

## Task 3: Specify and unify the knowledge-pack alias

**Files:**

- Modify: `crates/spur-analyst/tests/pack/mcp_tools_characterization.rs`
- Delete: `crates/spur-analyst/tests/snapshots/knowledge_context_pack_v1_shape.json`
- Modify: `crates/spur-analyst/src/mcp/tools/knowledge_context.rs`

- [ ] Replace `knowledge_context_pack_v1_response_shape_matches_snapshot` with `knowledge_context_pack_alias_matches_v2_behavior`. Dispatch both public names with the same v2 arguments, including all `graph_reasoning` flags and bounded path settings, normalize dynamic fixture paths, and assert equal responses.

- [ ] Assert the alias response contains the v2 sections (`graph_paths`, `risk_scorecard`, `community_context`, `temporal_context`, and `caveats`) even when a section is empty or unavailable. Keep the canonical v2 snapshot test.

- [ ] Tighten `knowledge_context_is_split_into_pack_service_and_thin_mcp_adapter` so it no longer requires the public alias to call `service::knowledge_context_pack`; it should require the adapter to remain thin and the public alias to share the v2 route.

- [ ] Run the alias test and confirm the old v1 response differs or ignores v2 arguments:

  ```bash
  scripts/spur-cargo test -p spur-analyst --test pack mcp_tools_characterization::knowledge_context_pack_alias_matches_v2_behavior -- --exact
  ```

- [ ] Commit the failing alias regressions and stale snapshot removal:

  ```bash
  git add crates/spur-analyst/tests/pack/mcp_tools_characterization.rs crates/spur-analyst/tests/snapshots/knowledge_context_pack_v1_shape.json
  git commit -m "test(spur-analyst): bd-1qd2l require v2 alias parity"
  ```

- [ ] Implement the deprecated public name as a direct call to the canonical adapter:

  ```rust
  pub async fn knowledge_context_pack(args: &Value) -> Result<Value, McpHandlerError> {
      knowledge_context_pack_2(args).await
  }
  ```

  Remove the now-unused `KnowledgeContextPackRequest` import. Keep the internal v1 service for non-MCP callers; only public MCP dispatch is unified.

- [ ] Run the characterization and v2 service suites:

  ```bash
  scripts/spur-cargo test -p spur-analyst --test pack mcp_tools_characterization
  scripts/spur-cargo test -p spur-analyst --test pack knowledge_context_pack_2
  ```

- [ ] Commit the alias implementation:

  ```bash
  git add crates/spur-analyst/src/mcp/tools/knowledge_context.rs
  git commit -m "fix(spur-analyst): bd-1qd2l route pack alias through v2"
  ```

## Task 4: Specify table-format completeness and incompatible-format errors

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs` (tests only in this task)
- Modify: `crates/spur-graph/tests/mcp_overlay_notfound_retry.rs`
- Modify: `crates/spur-core/tests/code_graph_e2e.rs`
- Modify: `crates/spur-core/tests/worker_server_dispatch.rs`

- [ ] Add a `code_symbol_search` table regression using multiple symbols from one file. Assert:

  - top-level `response_format == "table"`;
  - top-level `files == ["src/lib.rs"]`;
  - `candidates` has `cols` and `rows`, not an object array;
  - the `file` column contains interned integer indexes;
  - table serialization is smaller than the equivalent full response.

- [ ] Extend the dirty-worktree search/overlay regression to request table and assert the refreshed response remains table-shaped with `rebuild_status == "fresh"`.

- [ ] Add ambiguous-selector table regressions for callers, callees, and both subgraph backends. Each successful ambiguity response must contain `ambiguous: true`, table-shaped `candidates`, `response_format: "table"`, and top-level `files`.

- [ ] Add explicit `InvalidParams` regressions for:

  - `code_subgraph` with `format: "mermaid"` plus `response_format: "table"`, in both client and loaded-artifact paths;
  - `code_read_symbol` with `response_format: "table"`;
  - metadata-only `code_resolve`/`code_symbol_info` with an unsupported table request.

- [ ] Extend `tools_list_advertises_response_format_for_worker_code_graph_tools` so the worker `code_symbol_search` schema visibly advertises `full|compact|table`, and add one worker/brain dispatch assertion for the actual table shape.

- [ ] Run the focused tests and confirm the current implementation returns object candidates or silently accepts incompatible formats:

  ```bash
  scripts/spur-cargo test -p spur-graph --lib mcp::tests::code_search
  scripts/spur-cargo test -p spur-graph --lib mcp::tests::code_subgraph
  scripts/spur-cargo test -p spur-graph --test mcp_overlay_notfound_retry
  scripts/spur-cargo test -p spur-core --test code_graph_e2e table_format
  scripts/spur-cargo test -p spur-core --test worker_server_dispatch response_format
  ```

- [ ] Commit only the failing format regressions:

  ```bash
  git add crates/spur-graph/src/mcp/mod.rs crates/spur-graph/tests/mcp_overlay_notfound_retry.rs crates/spur-core/tests/code_graph_e2e.rs crates/spur-core/tests/worker_server_dispatch.rs
  git commit -m "test(spur-graph): bd-1qd2l cover table format branches"
  ```

## Task 5: Centralize candidate tables and enforce per-tool format capabilities

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`
- Modify: `crates/spur-core/src/worker_server.rs`

- [ ] Split response-format parsing by capability while preserving the omitted default as `Full`:

  - collection parser: `full|compact|table`;
  - source parser: `full|compact|source` (table must not be accepted);
  - metadata-object parser: `full|compact` (table/source must not be accepted).

  Keep error messages synchronized with the accepted variants. Use the source parser for `code_read_symbol` and the metadata parser inside `code_resolve` and `code_symbol_info`.

- [ ] Add a single candidate collection shaper. For table it must call the existing `candidate_table` and `table_response`; for full/compact it must preserve the existing object rows. Use it from:

  - `code_symbol_search` normal, overlay, and rebuild paths;
  - ambiguity returns in callers, callees, and both subgraph paths.

- [ ] Pass the already-parsed `ResponseFormat` into `code_search_body_for_client` rather than creating a table-shaped body and then losing the requested format during refresh. Preserve `CodeSearchBody.result`/`options` so response file OID analysis still uses exact search results.

- [ ] Validate the subgraph `format`/`response_format` pair before selector resolution and traversal. `mermaid + table` must fail consistently even when the selector is ambiguous.

- [ ] Add `response_format: Option<CodeResponseFormat>` to the worker `CodeSearchParams` schema mirror and update its field/tool descriptions to describe the interned candidate table. No runtime decoding change is required because worker dispatch forwards the original JSON object.

- [ ] Run all graph and core format tests:

  ```bash
  scripts/spur-cargo test -p spur-graph
  scripts/spur-cargo test -p spur-core --test code_graph_e2e
  scripts/spur-cargo test -p spur-core --test worker_server_dispatch
  ```

- [ ] Commit the format implementation:

  ```bash
  git add crates/spur-graph/src/mcp/mod.rs crates/spur-core/src/worker_server.rs
  git commit -m "fix(spur-graph): bd-1qd2l stabilize table responses"
  ```

## Task 6: Specify direction-specific unresolved defaults

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs` (tests and tool definitions)
- Modify: `crates/spur-core/tests/code_graph_e2e.rs`
- Modify: `crates/spur-core/tests/worker_server_dispatch.rs`

- [ ] Add a traversal fixture containing resolved and unresolved records, then assert:

  - omitted caller `include_unresolved` echoes `true` and includes unresolved caller rows;
  - omitted callee `include_unresolved` echoes `false` and filters unresolved callee rows;
  - omitted subgraph `include_unresolved` remains `false`;
  - explicit `false` overrides the caller default;
  - explicit `true` overrides the callee default;
  - full, compact, and table responses all echo the same effective boolean.

- [ ] Update schema tests so the graph `code_callers` property default is `true`, the callee default remains `false`, and worker tool descriptions state the direction-specific behavior.

- [ ] Run the focused tests and confirm the omitted caller case fails against the shared false default:

  ```bash
  scripts/spur-cargo test -p spur-graph --lib include_unresolved
  scripts/spur-cargo test -p spur-core --test code_graph_e2e unresolved_default
  scripts/spur-cargo test -p spur-core --test worker_server_dispatch unresolved_default
  ```

- [ ] Commit only the failing traversal-default regressions:

  ```bash
  git add crates/spur-graph/src/mcp/mod.rs crates/spur-core/tests/code_graph_e2e.rs crates/spur-core/tests/worker_server_dispatch.rs
  git commit -m "test(spur-graph): bd-1qd2l pin traversal defaults"
  ```

## Task 7: Implement direction-specific unresolved defaults and schemas

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`
- Modify: `crates/spur-core/src/worker_server.rs`

- [ ] Change the shared traversal request parser to take an explicit default:

  ```rust
  fn code_traversal_request(
      args: &Value,
      include_unresolved_default: bool,
  ) -> Result<CodeTraversalRequest, McpHandlerError>
  ```

  Pass `true` only from callers and `false` from callees and both subgraph backends.

- [ ] Update `code_callers_def` to advertise `default: true` and explain that unresolved rows are included by default. Retain `default: false` and filtering language for callees/subgraph.

- [ ] Make worker schemas directionally accurate. Prefer separate caller/callee schema structs with flattened common selector/format fields so each `include_unresolved` description can state its own default; do not lie through the current shared `CodeSymbolParams` documentation.

- [ ] Update worker tool descriptions and any characterization text that still says caller unresolved rows are hidden by default.

- [ ] Run graph/core tests:

  ```bash
  scripts/spur-cargo test -p spur-graph
  scripts/spur-cargo test -p spur-core --test code_graph_e2e
  scripts/spur-cargo test -p spur-core --test worker_server_dispatch
  ```

- [ ] Commit the traversal implementation:

  ```bash
  git add crates/spur-graph/src/mcp/mod.rs crates/spur-core/src/worker_server.rs
  git commit -m "fix(spur-graph): bd-1qd2l include unresolved callers"
  ```

## Task 8: Full verification and review handoff

**Files:**

- Modify only if verification exposes a defect in the files already scoped above.

- [ ] Format and reject whitespace errors:

  ```bash
  scripts/spur-cargo fmt --all
  git diff --check
  ```

- [ ] Run the complete affected test surfaces:

  ```bash
  scripts/spur-cargo test -p spur-analyst --test pack
  scripts/spur-cargo test -p spur-graph
  scripts/spur-cargo test -p spur-core --test code_graph_e2e
  scripts/spur-cargo test -p spur-core --test worker_server_dispatch
  ```

- [ ] Run remote lint, as required inside an agent worktree:

  ```bash
  SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-analyst -p spur-graph -p spur-core -- -D warnings
  ```

- [ ] Search the scoped public contracts for stale claims and inspect every hit:

  ```bash
  rg -n "Unresolved rows are hidden by default|include_unresolved=false|identical across all three formats|KnowledgeContextPackRequest" \
    crates/spur-graph/src/mcp crates/spur-analyst/src/mcp crates/spur-core/src/worker_server.rs
  ```

- [ ] Confirm `git status --short` contains only intended files, and inspect the cumulative diff from the delegated base.

- [ ] Use `spurpower-requesting-code-review` for a final review. Fix any correctness findings with focused tests, rerun the relevant commands, and commit final formatting/review changes if any:

  ```bash
  git add crates/spur-analyst crates/spur-graph crates/spur-core
  git commit -m "chore(spur-mcp): bd-1qd2l finish MCP safety verification"
  ```

  Skip this last commit when formatting/review creates no diff.

- [ ] Report the commit range, exact passing commands, any intentionally retained v1 internal service, and any remaining risk. Do not close `bd-1qd2l`; the brain performs review and lifecycle transition after integration.
