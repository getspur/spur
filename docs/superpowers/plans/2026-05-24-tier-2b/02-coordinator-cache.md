# t2b-coordinator-cache: pin artifact + cache `Arc<TemporalIndex>` in `RebuildCoordinator`

## Context

After `t2b-arc-owned`, `TemporalIndex` is lifetime-free, `Send + Sync`.

`RebuildCoordinator` at `crates/spur-mcp/src/server/handlers/rebuild_singleflight.rs` currently stores only `Weak<OnceCell<Arc<GraphIndexArtifact>>>` (line 30/33) keyed on `RebuildKey`, and reaps dead entries on every access — so artifacts are dropped the moment in-flight requests end.

The coordinator is a **process-wide singleton** constructed once in `crates/spur-mcp/src/server/mod.rs:240` and `Arc::clone`d into **nine** `code_graph` handlers (resolve, search, file_symbols, symbol_info, read_symbol, callers, callees, subgraph, symbol_history) via lines 386–454 of `code_graph.rs`. Pinning the artifact benefits all nine, not just `code_symbol_history`.

The two call sites that consume `symbol_history` today (post-Tier-1):

- `code_symbol_history_events` at `crates/spur-mcp/src/server/handlers/code_graph.rs:968-996`. Builds `TemporalIndex::new(artifact)` at line 976; calls `symbol_history(&index, ...)` at line 981.
- `resolve_symbol_as_of` at lines 1806–1849. Builds at line 1815; calls at line 1820.

## Goal

1. **Strong-ref LRU on the coordinator.** Add a `const CACHE_CAPACITY: usize = 1;` strong-ref LRU of `Arc<GraphIndexArtifact>` keyed on the existing `RebuildKey`, alongside the existing weak map. No env plumbing.

2. **Cache `Arc<TemporalIndex>` alongside the artifact.** Lazily compute and cache an `Arc<TemporalIndex>` keyed on the same `RebuildKey` (`OnceCell` on a small bundle struct OR a parallel map). Artifact + index form a bundle with the same lifetime.

3. **Public accessor.** Add a public method on `RebuildCoordinator` that returns `Arc<TemporalIndex>` for the most-recent retained artifact (build-and-cache on first miss). Accept the artifact `Arc` as the build seed; use the same `RebuildKey` scheme.

4. **Wire the two handlers.** In `code_symbol_history_events` and `resolve_symbol_as_of`, replace the local `TemporalIndex::new(artifact)` (lines 976, 1815) with a call into the coordinator. Pass `&temporal_index` to `symbol_history`. Outer function signatures stay the same.

5. **Build counter.** Add a `TemporalIndex` build counter mirroring the existing `build_invocations: AtomicUsize` (line 35 of `rebuild_singleflight.rs`) and `build_invocation_count()` accessor (line 87). Increment on actual build, not on cache hit. Gate the new counter and accessor under `#[cfg(any(test, feature = "test-support"))]`. Add a delegation on `crates/spur-mcp/src/server/test_helpers.rs` near the existing `build_invocation_count` delegation at line 143.

## Constraints

- Do not change the public MCP tool contract of `code_symbol_history`.
- Do not change `symbol_history`'s public signature.
- Cache key keeps the existing `RebuildKey` (head_oid + dirty_oid_set_hash) — staleness must remain structurally impossible.
- Preserve existing `Weak`-based concurrent dedup: concurrent calls with the same key still share one build.
- All existing tests in `crates/spur-mcp` MUST pass, including `rebuild_singleflight::tests::same_key_concurrent_calls_invoke_build_once`, `rebuild_singleflight::tests::dropped_completed_entry_is_rebuilt_on_next_access`, and the `code_graph_e2e` `code_symbol_history_*` tests at lines 1871–1934.

## Acceptance

- `cargo test -p spur-mcp` green.
- One-paragraph summary describing the bundle shape, the LRU placement, and how the build counter is wired.
