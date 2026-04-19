# Stream Pipeline (post-unification)

**Status:** Landed on `feat/stream-tab-unification` (2026-04-19).
**Related:**
- Spec: `docs/superpowers/specs/2026-04-19-stream-tab-unification-design.md`
- Plan: `docs/superpowers/plans/2026-04-19-stream-tab-unification.md`
- Audit: `docs/superpowers/notes/2026-04-19-stream-buffer-audit.md`

## TL;DR

`SpurEventBody::WorkerNotification` is the **single source of truth** for what a worker executor is doing. Both the brain session view and the DetailPane Stream tab consume it through the **same dispatcher** (`crates/spur-tui/src/components/react_trace/dispatch.rs`), so every ACP `SessionUpdate` variant the brain view handles automatically appears in the Stream tab with no duplicate plumbing.

## Flow

```
SpurEvent arrives at App::handle_spur_event
    │
    ├─► LineageProjection::apply
    │       │
    │       └─► ExecutorNode: counters only
    │           (tool_call_count, latest_tool_call, last_event_at,
    │            diff summaries, phase). NO stream_buffer writes
    │            from WorkerNotification post-Phase-3.
    │
    ├─► Orphan-drop guard
    │       │
    │       └─► if lineage.node(executor_id).is_none() → skip
    │           (matches brain view's "events before view construction
    │            are lost" fidelity ceiling)
    │
    ├─► WorkerStreams::route(executor_id, agent_name, &update)
    │       │
    │       └─► react_trace::dispatch::dispatch_session_update
    │               │
    │               └─► per-executor ReactTrace in
    │                   HashMap<String, ReactTrace>
    │
    └─► ExecutorRetryStarted → WorkerStreams::reset(executor_id)
        (mirrors lineage projection's stream_buffer.clear())
```

## Render path

```
Dashboard::render_with_lineage
    │
    └─► DetailPane::render(frame, area, node, badge, stream_trace)
            │
            ├─► if tab == Stream AND stream_trace.is_some()
            │       │
            │       └─► trace.render_compact(frame, body_area)
            │           (no block/border/title; DetailPane owns those)
            │
            └─► else (Stream tab with no trace, OR other tabs)
                    │
                    └─► placeholder / render_artifacts / ...
```

## Scroll routing

- `DetailPane::{scroll_up, scroll_down, scroll_to_top, scroll_to_bottom}` accept `Option<&mut ReactTrace>`.
- When Stream tab is active AND a trace exists → route to `trace.scroll_*` (uses `ScrollAnchor`).
- Otherwise mutate `DetailPane::scroll_offset` as before.
- `cycle_tab` resets only `DetailPane::scroll_offset` — the per-executor trace's `ScrollAnchor` is PRESERVED across tab switches, so scroll position survives when users tab away and back.

## Tick drive

`App::tick()` calls `self.worker_streams.tick_all()` unconditionally. This advances the spinner frame on every per-executor trace, matching brain view behavior (spinners on `ActStatus::Pending`/`InProgress` entries).

## `stream_buffer` is not a rendering input.

The `VecDeque<WorkerStreamEntry>` field remains declared on `ExecutorNode` for serde backward compatibility with pre-unification `session_metadata.json` files. No code path writes to it from `WorkerNotification` anymore. The `ExecutorRetryStarted` handler still calls `stream_buffer.clear()` defensively.

Future Phase 4 work may delete the field entirely (bundled with a `SessionHistory` format version bump) but that's deferred indefinitely.

## `WorkerStreams` invariants

1. **One trace per executor** — keyed by `ExecutorId.0` (String).
2. **AgentKind memoised** — so `reset` rebuilds the trace with the correct accent color without needing to peek inside `ReactTrace`.
3. **Orphan events dropped** — the App gate ensures `route()` is only called when `lineage.node()` has the executor. Prevents permanent `AgentKind::Generic` mis-coloring from pre-spawn events.
4. **Tool-depth namespace is per-executor** — each trace's subagent-nesting `HashMap<String, u8>` lives on `WorkerStreams`, not on the trace.

## Performance knobs (Phase 4, deferred)

- **PP3** — drop compact cache on focus change (~1 MB per unfocused trace freed).
- **PP4** — compact-mode entry cap (`MAX_COMPACT_ENTRIES`). Would cut worst-case memory ~5× and speed cold renders.
- **PP5** — `LineageProjection::node_by_str(&str)` to avoid per-event `ExecutorId(String)` clone.
- **PP2** — `ReactTrace::tick` early-return via cached `has_active_or_pending: bool`.

Trigger conditions for each are documented in the plan's Phase 4 section.

## Where each piece lives

| Concern | File |
|---|---|
| `SessionUpdate` → mutation logic | `crates/spur-tui/src/components/react_trace/dispatch.rs` |
| Compact render | `crates/spur-tui/src/components/react_trace/compact_render.rs` |
| Per-executor trace registry | `crates/spur-tui/src/worker_streams.rs` |
| Event routing + orphan gate | `crates/spur-tui/src/app.rs` (`handle_spur_event`) |
| Dashboard → DetailPane wiring | `crates/spur-tui/src/views/dashboard.rs` |
| DetailPane render + scroll | `crates/spur-tui/src/components/detail_pane.rs` |
| Brain view dispatch | `crates/spur-tui/src/views/session_detail.rs` (calls the same dispatcher) |
| Counter-only projection | `crates/spur-core/src/lineage/projection.rs` (`WorkerNotification` arm) |
| `AgentKind::from_name` | `crates/spur-acp/src/types.rs` |
| Criterion benchmarks | `crates/spur-tui/benches/compact_render.rs` |
