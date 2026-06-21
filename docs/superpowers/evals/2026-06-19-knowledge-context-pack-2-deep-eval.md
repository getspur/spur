# Knowledge Context Pack 2 Deep Evaluation - 2026-06-19

## Scope

This evaluation locks the v2 quality bar with the new integration fixture
`crates/spur-mcp/tests/knowledge_context_pack_2_eval.rs`. The harness builds
temporary Rust worktrees, writes exact graph artifacts, creates fixture analyst
DuckDB databases, and calls `knowledge_context_pack_2` through
`McpCallbackServer::__test_call_tool`.

The fixture harness is the authoritative deliverable. I did not run the optional
live MCP battery; live servers can lag the just-built source, while this test
executes the current source through the same JSON-RPC tool dispatch path.

## Coverage Matrix

| Matrix item | Scenario | Result |
| --- | --- | --- |
| G1 calls-only path rows | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` | PASS: every returned row has `relation:"calls"` and `edge_kind` in `calls` or `calls_dyn`; containment and import noise rows are excluded. |
| G1 calls_dyn-inclusive path | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` | PASS: the returned multi-hop path includes a `calls_dyn` hop. |
| G1 node-sequence dedup | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` | PASS: duplicate DB edges collapse to one full node sequence before `max_paths`. |
| G2 risk reconciliation fields | `ambiguous_sink_risk_reconciles_exact_inbound_and_bounds_popular_sink` | PASS: risk rows include `label_inbound`, `inbound_unresolved`, and `name_ambiguous`. |
| G2 ambiguous bare-name sink | `ambiguous_sink_risk_reconciles_exact_inbound_and_bounds_popular_sink` | PASS: `common_sink` is `posture:"leaf"` with scorecard `callers:0`, exact `label_inbound:31`, `inbound_unresolved:31`, `name_ambiguous:true`, and primary `impact.popular_sink:true`. |
| G2 unambiguous control | `ambiguous_sink_risk_reconciles_exact_inbound_and_bounds_popular_sink` | PASS: `control_leaf` has `label_inbound:1`, `inbound_unresolved:0`, and `name_ambiguous:false`. |
| G3 undirected traversal | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` and `disjoint_singletons_emit_single_no_path_caveat` | PASS: every path entry asserts `traversal:"undirected"`. |
| G3 component fields suppressed | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` | PASS: `community_context` includes `community_id` and omits `component_id` and `component_size`. |
| G5 confidence calibration | `confidence_calibration_spans_low_medium_high` | PASS: controlled fixture evidence produces low, medium, and high confidence. |
| G6 caveat dedup per source | `disjoint_singletons_emit_single_no_path_caveat` | PASS: repeated no-path targets for one source emit one `graph_path_unavailable` caveat. |
| G6 trimmed payload | `connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped` | PASS: component noise is trimmed from community rows; popular-sink expansion also suppresses caller neighbor payload. |
| Scope all/docs/code/graph | `scope_and_intent_variations_drive_defaults` | PASS: all returns code plus docs, docs returns only docs, code and graph return only code evidence. |
| Intent-driven graph defaults | `scope_and_intent_variations_drive_defaults` | PASS: review/code defaults request graph paths and risk; explain/code defaults do not request paths; docs scope suppresses risk. |
| Popular-sink impact boundary | `ambiguous_sink_risk_reconciles_exact_inbound_and_bounds_popular_sink` | PASS: popular sink is counted, reports summary `popular sink counted but not expanded`, and emits no caller neighbors. |
| Stale analyst hash suppression | `stale_analyst_hash_suppresses_graph_reasoning_sections` | PASS: mismatched analyst/exact graph hashes suppress paths, risk, and community sections and emit `analyst_graph_stale`. |

## Verification

Fresh scoped verification:

```text
scripts/spur-cargo test -p spur-mcp --test knowledge_context_pack_2_eval
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

```text
scripts/spur-cargo test -p spur-mcp knowledge_context
test worker_tools_list_includes_knowledge_context_pack ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out
```

No unfiltered `scripts/spur-cargo test -p spur-mcp` run was performed.

## Readiness Verdict

`knowledge_context_pack_2` is ready to hold the first-class quality bar covered
by this matrix. The integration fixture exercises the promoted v2 behavior
through public tool dispatch and did not surface a blocker requiring source
changes.

## Residual Gaps

- Recall remains BM25/macro-row dependent in this fixture. The eval validates
  packing, graph reasoning, risk reconciliation, and confidence calibration once
  evidence is retrieved; it does not prove semantic recall for vocabulary-mismatched
  production queries.
- The calls_dyn decision outcome is locked as: `edge_kind:"calls_dyn"` is a
  valid call-path row when `relation:"calls"`, and should not be excluded with
  containment/import/reference-other noise.
- The optional live MCP battery was not run. If run later, treat it as advisory
  only unless the live server binary is confirmed to match the just-built source.
