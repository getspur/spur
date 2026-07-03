# Lean MCP Tool Response Metadata Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close a schema gap that hides an already-working token-saving capability on `code_symbol_search`, then produce a verified (not assumed) audit of whether the same lean-metadata pattern is worth extending to the other 55 spur-mcp tools that don't yet support `response_format`.

**Architecture:** `crates/spur-graph/src/mcp/mod.rs` already implements a `ResponseFormat` enum (`Full`/`Compact`/`Table`/`Source`) and a `GraphResponseMetadata::insert_into_for_format` method that omits an 8-field graph-staleness trailer (`graph_content_hash`, `graph_index_version`, `graph_built_at`, `indexed_head_oid`, `worktree_head_oid`, `worktree_dirty`, `response_file_oids_match`, `rebuild_status` — roughly 94 tokens) unless something is actually stale. This is wired correctly for 5 of `spur-graph`'s 9 tools. `code_symbol_search`'s handler (`code_search_response`) already calls this same machinery correctly — confirmed by live tool call, passing `response_format: "table"` to `code_symbol_search` already omits the trailer today — but the tool's advertised `input_schema` never lists `response_format`, so no caller can discover the parameter exists. Task 1 fixes that schema gap. Task 2 is an audit, not a blind rollout: an earlier hypothesis in this investigation (rank the other 55 tools by handler line count as a proxy for "response bloat") was checked against real handler source and falsified — `handle_get_loop_status` and `handle_submit_plan` in `crates/spur-core/src/server/handlers/plan.rs` are already lean; their length comes from validation/branching logic, not redundant response fields. Task 2 replaces that guess with a real, source-grounded audit of named candidates before any further code changes are planned.

**Tech Stack:** Rust, `serde_json`, existing `rmcp` MCP tool-call test harness in `crates/spur-core/tests/worker_server_dispatch.rs`.

---

### Task 1: Advertise `response_format` on `code_symbol_search`

**Files:**
- Modify: `crates/spur-graph/src/mcp/mod.rs:731-770` (`code_symbol_search_def`)
- Modify (test): `crates/spur-core/tests/worker_server_dispatch.rs:453-472` (`tools_list_advertises_response_format_for_worker_code_graph_tools`)

**Current schema** (`crates/spur-graph/src/mcp/mod.rs:731-770`):

```rust
fn code_symbol_search_def() -> ToolDefinition {
    ToolDefinition {
        name: "code_symbol_search".into(),
        description: "Search the worktree graph artifact for symbols by NAME (exact/prefix/substring). Lexical retrieval over symbol identifiers, not content — returns ranked candidate symbols. For concept/content/natural-language retrieval over docs + code bodies, use code_semantic_search instead. Legacy alias: code_search.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Search term. Non-empty."
                },
                "mode": {
                    "type": "string",
                    "enum": ["exact", "prefix", "substring"],
                    "default": "substring"
                },
                "symbol_kind": {
                    "type": "string",
                    "description": "Optional filter on the artifact's symbol_kind, e.g. function, method, struct, enum, mcp_tool."
                },
                "file": {
                    "type": "string",
                    "description": "Optional exact worktree-relative file path. Mutually exclusive with file_glob."
                },
                "file_glob": {
                    "type": "string",
                    "description": "Optional glob over worktree-relative file_path (e.g. 'crates/spur-mcp/**/*.rs'). Mutually exclusive with file."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 20
                }
            },
            "required": ["query"]
        }),
    }
}
```

The handler behind this tool, `code_search_response` (`crates/spur-graph/src/mcp/mod.rs:881-968`), already does the right thing — it calls `ResponseFormat::parse(args)` and threads the result into `analysis.metadata.insert_into_for_format(&mut body, response_format)`. Live-tested this session: calling `code_symbol_search` with `{"query": "ToolResponse", "response_format": "table"}` already omits the `graph_content_hash`/`indexed_head_oid`/`worktree_head_oid`/`worktree_dirty`/`response_file_oids_match`/`rebuild_status` trailer today, because `rmcp` does not enforce `additionalProperties: false` on undeclared schema properties. **This means the fix here is schema-only — no handler behavior changes, and the "already works at runtime" fact is exactly why the failing test below must assert on the *declared schema*, not on response behavior (behavior already passes).**

Unlike `code_callers`/`code_callees`/`code_file_symbols`, `code_symbol_search` has no row-interning "table" behavior — its `candidates` array shape is identical across all three formats. Only the staleness trailer is affected. The new schema description must say this accurately (do not copy the `code_callers` description verbatim — it references table row interning that doesn't apply here).

There is already an exact-fit precedent test covering this gap by omission. `crates/spur-core/tests/worker_server_dispatch.rs:453-472`:

```rust
async fn tools_list_advertises_response_format_for_worker_code_graph_tools() {
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/list", json!({})).await;
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools array present");

    for tool_name in [
        "code_file_symbols",
        "code_callers",
        "code_callees",
        "code_subgraph",
    ] {
        assert_worker_response_format_enum(tools, tool_name, &["full", "compact", "table"]);
    }
    assert_worker_response_format_enum(tools, "code_read_symbol", &["full", "compact", "source"]);

    server.shutdown(Duration::from_secs(5)).await;
}
```

This test enumerates exactly the 5 tools that currently advertise `response_format` and silently omits `code_symbol_search`. `code_symbol_search` is confirmed present in this same worker-curated catalog by the sibling test `tools_list_returns_curated_worker_tools_including_code_graph_reads` (same file, lines 407-450, `expected` array includes `"code_symbol_search"`), so this file is the correct catalog to assert against — no separate brain-side test is needed since both catalogs resolve the same `code_symbol_search_def()`. The assertion helper `assert_worker_response_format_enum` (same file, lines 503-523) already does exactly what's needed: looks up the tool by name in the `tools/list` JSON-RPC response, asserts `inputSchema.properties.response_format` is a string schema, and compares its `enum` array.

- [ ] **Step 1: Write the failing test**

Edit `crates/spur-core/tests/worker_server_dispatch.rs`, adding `"code_symbol_search"` to the existing loop array in `tools_list_advertises_response_format_for_worker_code_graph_tools` (it takes the same `["full", "compact", "table"]` enum as the other 4 — `code_symbol_search`'s handler calls `ResponseFormat::parse`, not `parse_allowing_source`, so it does not support `"source"` the way `code_read_symbol` does):

```rust
    for tool_name in [
        "code_file_symbols",
        "code_callers",
        "code_callees",
        "code_subgraph",
        "code_symbol_search",
    ] {
        assert_worker_response_format_enum(tools, tool_name, &["full", "compact", "table"]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `scripts/spur-cargo test -p spur-core --test worker_server_dispatch tools_list_advertises_response_format_for_worker_code_graph_tools`

Expected: FAIL — `assert_worker_response_format_enum` panics with `code_symbol_search must define response_format in worker tools/list inputSchema`, because `code_symbol_search_def()`'s schema does not yet declare the property.

- [ ] **Step 3: Add `response_format` to the schema**

Edit `crates/spur-graph/src/mcp/mod.rs`, inside `code_symbol_search_def()`'s `properties` object, adding a sibling to `"limit"`:

```rust
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 20
                },
                "response_format": {
                    "type": "string",
                    "enum": ["full", "compact", "table"],
                    "description": "Output shape. full is the default response including graph staleness metadata (graph_content_hash, worktree_head_oid, etc.); compact and table both omit those fields when nothing is stale. candidates rows are identical across all three formats — code_symbol_search has no row-interning table behavior."
                }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `scripts/spur-cargo test -p spur-core --test worker_server_dispatch tools_list_advertises_response_format_for_worker_code_graph_tools`

Expected: PASS

- [ ] **Step 5: Run the full spur-graph and spur-core suites to check for schema-stability regressions**

Run: `scripts/spur-cargo test -p spur-graph -p spur-core`

Expected: PASS. `tools_list_returns_curated_worker_tools_including_code_graph_reads` (same file) asserts an exact tool-name set, not schema contents, so it is unaffected by this change. If any other schema-snapshot test elsewhere pins the old `code_symbol_search` schema verbatim, update that snapshot's expected `properties` set to include `response_format` — do not weaken the test's assertion style to make it pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-graph/src/mcp/mod.rs crates/spur-core/tests/worker_server_dispatch.rs
git commit -m "$(cat <<'EOF'
test(spur-graph): pin code_symbol_search response_format schema in worker catalog

fix(spur-graph): advertise response_format on code_symbol_search

The handler already parsed and honored response_format (confirmed live:
compact/table already omit the ~94-token graph-staleness trailer), but
the tool's input_schema never declared the parameter, so no caller could
discover it.
EOF
)"
```

(Per this repo's commit convention, split into a `test(...)` commit followed by a `fix(...)` commit if your workflow enforces separate commits per type — both message subjects are provided above for that case.)

---

### Task 2: Audit — is the lean-metadata pattern worth extending further?

**Files:**
- Create: `docs/superpowers/specs/2026-07-03-mcp-response-audit-findings.md`

This task produces a **findings document**, not code changes. Do not write speculative `response_format` plumbing for any tool below until its row in the findings table is filled in with a real measurement — the whole point of this task is that Task 1's original justification (rank tools by handler line count) was already tried and falsified against real source in this session (see Architecture section above). Guessing again without measuring would repeat that mistake.

**Method — repeat for each candidate below:**

1. Read the handler's actual response-construction code (not just its outer function — follow delegation, e.g. `handle_get_plan_status` at `crates/spur-core/src/server/handlers/plan.rs:1899-1932` delegates to `get_plan_status` at `crates/spur-core/src/handlers.rs:145-174`, which delegates further to `crate::plan::build_plan_status` — read that too).
2. Capture one real response payload (call the live tool, or construct a realistic fixture matching the real field shape — do not synthesize an idealized/simplified shape).
3. Identify concretely which fields are (a) always the same value in the common case, (b) `null`/default padding on most rows, or (c) prose/data duplicated elsewhere in the same payload. Name the exact fields — "seems verbose" is not a finding.
4. Measure token impact the same way this session did: write both the real (pretty) JSON and a hand-trimmed version removing only the fields identified in step 3 to a temp file, and compare with `npx -y @toon-format/cli -e --stats -o /dev/null <file>.json` (reads the `~N (JSON)` token estimate; ignore the TOON-encoding half of that tool's output — this step is not about adopting TOON).
5. Record the result as a row in the findings table below, with a clear verdict: `worth a response_format task` (savings materially reduce a call site or high-call-volume tool) or `not worth it` (already lean, or savings are marginal like the earlier `to_string_pretty`→`to_string` finding, which measured only 1-4%).

**Candidates to audit (already identified as plausible, not yet verified):**

| Candidate | Entry point | Why plausible |
|---|---|---|
| `get_plan_status` | `crates/spur-core/src/handlers.rs:145-174` → `crate::plan::build_plan_status` | Polling-style tool (called repeatedly during a plan run, same failure mode as the graph staleness trailer — repeated calls compounding a fixed per-call cost); appends `plan_state_freshness`/`recent_outcomes`/`stuck_tasks` on top of `build_plan_status`'s own output, which has not yet been read in this investigation. |
| `list_issues` | `crates/spur-pm/src/mcp/mod.rs` (def at line ~472, per `code_symbol_search symbol_kind=mcp_tool` results this session) | Row-repetition risk similar to `code_callers`' pre-`table`-format shape — has not yet been read. |
| `graph_plan` / `graph_insights` / `graph_alerts` | `crates/spur-pm/src/mcp/mod.rs` (defs at lines 611/627/643 per this session's tool listing) | Named similarly to the already-fixed `code_*` graph tools; worth checking whether they independently reinvented a verbose trailer. |
| `external_code_callers` / `external_code_callees` | `crates/spur-context-service/src/mcp.rs:1331/1359` and `crates/spur-core/src/mcp/context_service.rs:206/234` | Directly parallel to `code_callers`/`code_callees`, which already got the `table` treatment on the worktree-graph side — check whether the external-package equivalents independently need the same fix or already inherited it. |

**Explicitly ruled out by this session's direct source reads (do not re-audit without new evidence):**

- `handle_get_loop_status` (`crates/spur-core/src/server/handlers/plan.rs:1772-1897`) — already a lean 9-field object (`loop_id`, `issue_id`, `spec`, `recent_runs`, `consecutive_failures`, `effective_interval_secs`, `backoff_active`, `paused`, `next_run`); its 125-line body is validation/data-loading, not response bloat.
- `handle_submit_plan` (`crates/spur-core/src/server/handlers/plan.rs:987-1226`) — returns a hand-composed prose string plus a `serde_json::to_string` (already compact, not pretty) `task_map_json`; its 239-line body is idempotency-key/validation logic, not response bloat.

- [ ] **Step 1: Read and measure each candidate in the table above per the method**

- [ ] **Step 2: Write the findings document**

Create `docs/superpowers/specs/2026-07-03-mcp-response-audit-findings.md` with one section per candidate: entry point, actual response shape observed, specific redundant fields (or "none found"), measured token delta, and verdict. This document becomes the spec input for a follow-up plan — do not fold speculative Task 3+ code changes into this plan based on an unverified guess.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-07-03-mcp-response-audit-findings.md
git commit -m "$(cat <<'EOF'
docs(spur-mcp): audit findings for lean-response-metadata candidates beyond code_symbol_search

EOF
)"
```
