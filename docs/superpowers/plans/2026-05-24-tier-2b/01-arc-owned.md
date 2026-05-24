# t2b-arc-owned: drop the `<'a>` from `TemporalIndex` so it can live in an `Arc`

## Context

`crates/spur-graph/src/temporal.rs` defines `TemporalIndex` with a single lifetime parameter that borrows the artifact (line 26 onward). HashMap keys are `&'a str` borrowed from the artifact. `TemporalHistorySource` at line 173 has the same lifetime, and the `Index` variant holds `&'a TemporalIndex<'a>`. To cache `TemporalIndex` inside an `Arc` that lives across MCP requests, the lifetime parameter must go.

`TemporalIndex` is referenced in **only four files workspace-wide**:

- `crates/spur-graph/src/temporal.rs` — definition plus 12 internal sites.
- `crates/spur-graph/benches/incremental.rs` lines 11, 648, 668 — bench imports plus two construction sites.
- `crates/spur-mcp/src/server/handlers/code_graph.rs` lines 16, 976, 1815 — import plus the two handler construction sites.

No other crate consumes it.

## Goal

1. **Drop the lifetime parameter from `TemporalIndex`.** Pick one of two shapes — smaller diff wins:
   - **Shape A:** hold an `Arc<GraphIndexArtifact>` internally and re-borrow `&str` slices via accessor methods.
   - **Shape B:** switch HashMap keys to `Arc<str>`.
   Either must avoid per-lookup allocation in `edges_for_stable_symbol_id`, `commit_position`, `snapshot_keys_for_symbol_indexed`.

2. **Adjust `TemporalHistorySource`** so the `Index` variant takes `&TemporalIndex` (no lifetime). Keep the `Artifact` variant taking `&GraphIndexArtifact`. Preserve both `From` impls so `symbol_history`'s generic API is unchanged for existing callers.

3. **Internal helpers must compile against the lifetime-free shape:** `symbol_history_indexed`, `close_symbol_history_chain`, `snapshot_keys_for_symbol_indexed`, `expand_stable_symbol_snapshots`, `close_rename_chain_indexed`, `seed_symbol_history_keys`.

4. **`TemporalIndex::new` accepts `Arc<GraphIndexArtifact>`** (preferred) and returns an owned `TemporalIndex` that holds its artifact via `Arc`. `TemporalIndex` must be `Send + Sync`.

5. **Update the four downstream construction sites:**
   - `crates/spur-graph/benches/incremental.rs:648` — wrap with `Arc::new(graph)`.
   - `crates/spur-graph/benches/incremental.rs:668` — same.
   - `crates/spur-mcp/src/server/handlers/code_graph.rs:976` — same.
   - `crates/spur-mcp/src/server/handlers/code_graph.rs:1815` — same.

## Constraints

- `symbol_history`'s public signature stays generic over `S: Into<TemporalHistorySource<'a>>`.
- Existing tests in `crates/spur-graph` MUST pass with no edits: `tests/snapshot_rename_edges.rs`, `tests/modify_chain_continuity.rs`, `tests/temporal_resolution.rs`, plus in-file tests at lines 1068, 1082, 1090, 1105.
- Do not change parquet read/write or the `GraphIndexArtifact` schema.
- No new external crates (no `ouroboros`, no `yoke`). Only `std::sync::Arc` and optionally `Arc<str>`.
- Do not change the `bench_history_walk_50k_snapshots` budget.
- No unsafe.

## Acceptance

- `cargo test -p spur-graph` green.
- `cargo bench -p spur-graph --no-run` compiles.
- `cargo test -p spur-mcp` green.
- One-sentence rationale stating which ownership shape (A or B) was chosen and why.
