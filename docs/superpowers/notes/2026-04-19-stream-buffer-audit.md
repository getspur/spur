# stream_buffer Audit — 2026-04-19

Pre-Phase-3 audit of every consumer of `ExecutorNode.stream_buffer`.
Run against `feat/stream-tab-unification` at commit `64ada59`.

## Hits

| File | Line | Role |
|---|---|---|
| `crates/spur-core/src/lineage/types.rs` | 20 | doc comment (reference in `WorkerStreamEntry` docstring) |
| `crates/spur-core/src/lineage/types.rs` | 113 | **declaration** — `pub stream_buffer: VecDeque<WorkerStreamEntry>` on `ExecutorNode` |
| `crates/spur-core/src/lineage/projection.rs` | 119 | **writer** — initializer (`VecDeque::new()`) when constructing a fresh `ExecutorNode` on `ExecutorSpawned` |
| `crates/spur-core/src/lineage/projection.rs` | 268 | **writer** — `node.stream_buffer.clear()` on `ExecutorRetryStarted` |
| `crates/spur-core/src/lineage/projection.rs` | 309–312 | **writer** — eviction + push_back in the `WorkerNotification` handler (the lossy projection arm that Phase 3 will narrow) |
| `crates/spur-core/src/lineage/adapter.rs` | 39, 81 | **writer** — initializer (`VecDeque::new()`) when building nodes from external inputs (non-projection paths) |
| `crates/spur-acp/src/domain/events.rs` | 523 | doc comment (mentions `stream_buffer` as the downstream destination of `WorkerNotification` in the `SpurEventBody` docstring) |
| `crates/spur-tui/src/components/detail_pane.rs` | 178, 188 | **reader-render** — `render_stream` reads `node.stream_buffer` for the current Stream tab. To be retired in Task 3.2. |

## Classification summary

| Role | Count | Files |
|---|---|---|
| declaration | 1 | `types.rs:113` |
| writer (initializer) | 3 | `projection.rs:119`, `adapter.rs:39`, `adapter.rs:81` |
| writer (clear on retry) | 1 | `projection.rs:268` |
| writer (live WorkerNotification) | 1 | `projection.rs:309-312` |
| reader-render | 1 | `detail_pane.rs:178,188` |
| reader-summary | **0** | — |
| test | 0 in production code |
| doc comment | 2 | `types.rs:20`, `events.rs:523` |

## Conclusion

- **No `reader-summary` consumers exist.** The only reader is `DetailPane::render_stream`, which Task 3.2 retires.
- **Phase 3 write-removal is safe.** Narrowing `projection.rs:268-312` to stop writing (Task 3.1) leaves only initializer writes in `projection.rs:119` and `adapter.rs:39,81`. Those keep the field valid (empty `VecDeque`) for serde compatibility with existing `session_metadata.json` files.
- **Doc comments at `types.rs:20` and `events.rs:523` become stale** once Task 3.1 lands. They should be updated as part of Task 3.1 or Task 3.3.
- **Phase 4 deletion of the type** is blocked only by the existing initializers — removing them requires a `SessionHistory` format bump. Defer indefinitely.
