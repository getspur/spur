# t2b-cache-test: integration test asserting the cache works

## Context

After `t2b-coordinator-cache`, `RebuildCoordinator` exposes a `TemporalIndex`-build counter (gated under `#[cfg(any(test, feature = "test-support"))]`) mirroring the existing `build_invocations` at `crates/spur-mcp/src/server/handlers/rebuild_singleflight.rs:35` and `build_invocation_count()` at line 87, with a delegation on `test_helpers.rs`.

## Goal

Add a new integration test in `crates/spur-mcp/tests/code_graph_e2e.rs` near the existing `code_symbol_history_*` tests (around lines 1871–1934). The test must:

1. Issue a `code_symbol_history` request against a fixture artifact.
2. Issue a second `code_symbol_history` request against the **same** artifact.
3. Assert the `TemporalIndex`-build counter is **exactly 1** after both calls (i.e. the second request hit the cache).

Reuse the e2e harness and fixture builders used by:

- `code_symbol_history_returns_rename_chain` (line 1871).
- `code_symbol_history_returns_empty_when_no_snapshots` (line 1924).

## Constraints

- Add only the new test, plus any minimal pub-crate accessor needed to read the counter from the test.
- Any new accessor must be gated under `#[cfg(any(test, feature = "test-support"))]`.
- Deterministic: no sleeps, no flakiness; reuse existing fixtures.
- Do not change production logic.

## Acceptance

- `cargo test -p spur-mcp --test code_graph_e2e` green.
- One-sentence rationale describing the assertion and how it proves the cache works.
