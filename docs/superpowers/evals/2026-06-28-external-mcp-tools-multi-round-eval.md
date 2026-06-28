# External MCP Tools Multi-Round Evaluation - 2026-06-28

## Scope

This evaluation hardens the `external_*` MCP tool quality bar with two
deterministic tests in `crates/spur-context-service/tests/mcp_test.rs`.

The fixture uses an in-memory DuckDB package graph plus the fake catalog and job
store already used by the context-service MCP tests. It avoids network and AWS
dependencies while exercising the same public handler functions that serve the
MCP surface.

## Coverage Matrix

| Matrix item | Scenario | Result |
| --- | --- | --- |
| E1 knowledge-to-precision handoff | `external_tools_support_multi_round_agent_eval_flow` | PASS: `external_knowledge_context` returns a package selector and `next` entries for `external_code_read`, `external_code_callers`, and `external_code_callees`. |
| E2 package selector source read | `external_tools_support_multi_round_agent_eval_flow` | PASS: the selector from the pack reads the expected source body through `external_code_read`. |
| E3 callee unresolved toggle | `external_tools_support_multi_round_agent_eval_flow` | PASS: `include_unresolved:false` returns only resolved callees; `include_unresolved:true` returns the cross-package unresolved edge and sample. |
| E4 URI carry-over | `external_tools_support_multi_round_agent_eval_flow` | PASS: a `pkg-symbol://...` URI returned by `external_code_search` can be carried into `external_code_read`. |
| E5 caller edge-kind counts | `external_tools_support_multi_round_agent_eval_flow` | PASS: caller evaluation covers `calls`, `calls_dyn`, `references_hof`, and unresolved rows. |
| E6 cold index queue | `external_index_status_supports_cold_index_then_retry_eval_flow` | PASS: `external_index` queues a missing revision and starts exactly one fake execution. |
| E7 retry-before-index guard | `external_index_status_supports_cold_index_then_retry_eval_flow` | PASS: querying the cold revision before catalog population returns `NotFound`. |
| E8 status repair | `external_index_status_supports_cold_index_then_retry_eval_flow` | PASS: stale queued job status is repaired from a succeeded execution outcome and exposes snapshot/row counts. |
| E9 retry-after-index | `external_index_status_supports_cold_index_then_retry_eval_flow` | PASS: once the fixture catalog/query graph is moved to the indexed revision, `external_code_search` resolves the package selector. |

## Live MCP Sample

I also ran a non-authoritative live sample against indexed `serde` data:

- `external_knowledge_context({ package:"serde", query:"Deserialize deserialize callers callees" })`
  returned `pkg:serde@1.0.197::Deserialize::deserialize` with `next` entries
  for read, callers, and callees.
- `external_code_read` on that selector returned `src/de/mod.rs` lines 538-548.
- `external_code_callers(include_unresolved:true)` reported `calls:42` and
  `unresolved:38`, with resolved and unresolved caller rows.
- `external_code_callees(include_unresolved:false)` reported no callees for the
  trait method declaration.
- `external_index_status({ job_id:"external-eval-missing-job" })` returned
  `status:"not_found"`.

Treat the live sample as advisory. The deterministic fixture tests are the
regression guard because they run against the current source without depending
on production indexed package state.

## Verification

Fresh scoped verification:

```text
scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --features lambda --test mcp_test eval_flow
test external_index_status_supports_cold_index_then_retry_eval_flow ... ok
test external_tools_support_multi_round_agent_eval_flow ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out
```

The run emitted an existing warning from `src/main.rs`:
`unused import: std::sync::Mutex`.

Full context-service MCP test target:

```text
scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --features lambda --test mcp_test
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The full run emitted the same existing `src/main.rs` unused-import warning.

## Readiness Verdict

The external MCP tools now have a deterministic multi-round evaluation covering
the agent path from package discovery through source reads, graph edges,
selector/URI carry-over, cold indexing, status repair, and retry after index
availability.

## Residual Gaps

- The fixture validates tool contracts and multi-round flow, not production
  recall quality across large third-party packages.
- The live sample did not call `external_index` because that can enqueue real
  infrastructure work. Cold-index behavior is covered by the fake job store.
