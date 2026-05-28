# RCA: Brain-Side Continuation Drop After Detached Delegation Completion

Date: 2026-05-28

Subject delegation: `9ca42625-80a1-43c9-b735-0380ea23f778`

Comparator delegation: `507323cd-91a0-4aa5-88b5-e58b0f1b201f`

Brain session: `edd0d4684258d6b8`

Event stream: `.spur/events/12615-1779955905291-0.ndjson`

Log: `.spur/logs/spur.log.2026-05-28`

## 1. Executive Summary

The failed auto-continuation is **not** explained by a brain-side `tokio::broadcast::Receiver` missing `DelegationCompleted`.

The continuation path does not subscribe to the durable event stream or to the event broadcast at all. `DelegationCompleted` is a sibling observable emitted to the event funnel; the model-visible continuation follows a separate path:

`delegate_to_worker` detached slow path -> `spawn_result_collector` -> `build_detached_continuation` -> injected `DetachedContinuationCtx::on_complete` -> `report_detached_completion` -> bounded `mpsc::Sender<InteractiveInput>` -> `run_interactive` -> `BrainScheduler::push_continuation` -> `BrainScheduler::next` -> `PromptDispatched(turn_kind=continuation_only)`.

The exact subscriber for continuation injection is the `mpsc::Receiver<InteractiveInput>` owned by `Orchestrator::run_interactive`, not a broadcast receiver and not an ndjson poller. The producer uses `try_send`; if the bounded channel is full, it pushes the continuation into an overflow deque. That overflow deque is drained only at the top of the `run_interactive` loop.

The event evidence shows the failed delegation emitted `DelegationCompleted` at seq 3036, then no `IssuesLoaded`, no `GraphAlertsSummary`, no `ContinuationDropped`, no `ContinuationDeferred`, and no continuation prompt before the next user prompt at seq 3037. The logs show the same read-audit warning shape for both delegations and no logged `RecvError::Lagged`, `broadcast lagged`, or `channel_closed` around the failed completion.

Most likely failure mode: **(e) overflow-latency / idle-loop wakeup gap on the continuation ingress**. A detached completion can be accepted into the overflow deque instead of waking the idle `run_interactive` receiver; because the overflow is only drained when the loop re-enters, the continuation does not fire until some later input wakes the loop. In this incident, the next input was user text; `BrainScheduler::next` gives pending user input precedence over pending continuations, producing `PromptDispatched { turn_kind: "user_only", continuations_count: 0 }` at seq 3037.

This classification is stronger than the original broadcast-lag hypothesis because the graph-backed code path proves continuation delivery is mpsc-based, and the runtime event/log evidence contains no broadcast-lag marker. It is still not absolute proof that the failed continuation entered overflow, because `A_report_detached_completion` debug logs were not emitted at the active log level. The follow-up test below would disambiguate that last gap directly.

## 2. Graph-First Method

I used the `spur-mcp` code graph before filesystem search, per the task constraint.

Initial `code_search` terms:

- `PromptDispatched`: no code symbol candidates; later located by targeted read in `interactive_loop.rs`.
- `DelegationCompleted`: documentation section candidates only; implementation located after graph path identified `finalize`.
- `continuation`: relevant symbols included:
  - `crates/spur-core/src/continuation_bridge.rs::report_detached_completion`, `graph://symbol/80fb58ee3e53727e`
  - `crates/spur-core/src/scheduler.rs::impl BrainScheduler::push_continuation`, `graph://symbol/39be6ec0df7b74e2`
  - `crates/spur-core/src/scheduler.rs::impl BrainScheduler::drain_continuations_for_delivery`, `graph://symbol/97be9d2f228e5a43`
  - `crates/spur-core/src/orchestrator/support.rs::impl Orchestrator::set_continuation_tx`, `graph://symbol/ba081507f8188dc1`
  - `crates/spur-core/src/orchestrator/support.rs::impl Orchestrator::build_continuation_ctx`, `graph://symbol/a0e12e98a8e36942`
  - `crates/spur-mcp/src/server/types.rs::build_detached_continuation`, `graph://symbol/8e69e56b473ddf4d`
- `interactive_loop`: `crates/spur-core/src/orchestrator/interactive_loop.rs::impl Orchestrator::run_interactive`, `graph://symbol/d1e4d3f22a5a6fe4`
- `IssuesLoaded`: no implementation symbol, later found in `refresh_pm_state`.
- `GraphAlertsSummary`: no implementation symbol, later found in `refresh_pm_state`.

Graph calls used to trace the path:

- `code_callers(report_detached_completion)` returned `build_continuation_ctx` as the production caller: `crates/spur-core/src/orchestrator/support.rs:88-128`, `graph://symbol/a0e12e98a8e36942`.
- `code_callers(build_detached_continuation)` returned `spawn_result_collector` as the production caller: `crates/spur-mcp/src/server/handlers/delegation.rs:25-144`, `graph://symbol/d52f0b580b7ce9ba`.
- `code_callers(spawn_result_collector)` returned `handle_delegate_to_worker` and `handle_delegate_parallel`: `crates/spur-mcp/src/server/handlers/delegation.rs:442-627`, `graph://symbol/0f25fde07ccd2a76`; `crates/spur-mcp/src/server/handlers/delegation.rs:629-806`, `graph://symbol/6a2610b69729a7cf`.
- `code_callers(BrainScheduler::push_continuation)` included the production unresolved caller `run_interactive`: `crates/spur-core/src/orchestrator/interactive_loop.rs:129-1580`, `graph://symbol/d1e4d3f22a5a6fe4`.
- `code_callers(BrainScheduler::next)` also included `run_interactive`, with the caveat that `next` is a popular method name and the graph returned many unrelated unresolved rows. The relevant production row is still `run_interactive`, `graph://symbol/d1e4d3f22a5a6fe4`.

## 3. Event Evidence

### 3.1 Worked Delegation

The comparator delegation completed and immediately produced the observed continuation-only turn:

```text
.spur/events/12615-1779955905291-0.ndjson:1981
seq=1980 DelegationCompleted worker_session=71d7c122-0340-4b04-8364-9ab2716bd19a status=Success

.spur/events/12615-1779955905291-0.ndjson:1982
seq=1981 IssuesLoaded

.spur/events/12615-1779955905291-0.ndjson:1983
seq=1982 GraphAlertsSummary total=0 critical=0 warning=0

.spur/events/12615-1779955905291-0.ndjson:1984
seq=1983 PromptDispatched session=edd0d4684258d6b8 turn_kind=continuation_only continuations_count=1
```

Nearby pre-completion events show the worker was still streaming immediately before completion:

```text
.spur/events/12615-1779955905291-0.ndjson:1976-1980
seq=1975..1979 WorkerNotification from executor_id=71d7c122-0340-4b04-8364-9ab2716bd19a
```

The log confirms an issue refresh occurred at the same second as the successful auto-fire:

```text
.spur/logs/spur.log.2026-05-28:2710
2026-05-28T08:44:21.756923Z INFO spur_core::orchestrator::pm_bridge: Loaded open issues from beads count=498
```

### 3.2 Failed Delegation

The failed delegation reached the durable event stream:

```text
.spur/events/12615-1779955905291-0.ndjson:3037
seq=3036 DelegationCompleted worker_session=82099be3-e4cd-4336-ab62-b0830de9b8dd status=Success
```

No continuation prompt followed. The next dispatch was a user prompt:

```text
.spur/events/12615-1779955905291-0.ndjson:3038
seq=3037 PromptDispatched session=edd0d4684258d6b8 turn_kind=user_only continuations_count=0
```

Nearby pre-completion events again show normal worker streaming and a final usage update:

```text
.spur/events/12615-1779955905291-0.ndjson:3031-3036
seq=3030..3035 WorkerNotification from executor_id=82099be3-e4cd-4336-ab62-b0830de9b8dd
```

The failed case did not emit `IssuesLoaded` or `GraphAlertsSummary` between `DelegationCompleted` and the next prompt:

```text
.spur/events/12615-1779955905291-0.ndjson:3037-3046
seq=3036 DelegationCompleted
seq=3037 PromptDispatched user_only continuations_count=0
seq=3038..3045 AgentNotification for the user turn
```

### 3.3 Drop Signals

I searched the event stream for `Lagged`, `RecvError`, `broadcast lagged`, `channel_closed`, `ContinuationDropped`, `ContinuationDeferred`, and session restart markers. No smoking-gun event appears between the two relevant seq ranges. The only broad event-stream match for those terms was an initial `IssuesLoaded` payload and later worker notifications containing this investigation's own command text, not a SPUR runtime drop event.

The logs likewise contain no `RecvError`, `Lagged`, `broadcast lagged`, or `channel_closed` hit around the failed completion. The relevant targeted log hits were:

```text
.spur/logs/spur.log.2026-05-28:2633
ReadAggregate audit dropped ... delegation_id=507323cd-91a0-4aa5-88b5-e58b0f1b201f

.spur/logs/spur.log.2026-05-28:3368
ReadAggregate audit dropped ... delegation_id=9ca42625-80a1-43c9-b735-0380ea23f778
```

That warning is present for both delegations and is therefore not discriminating.

The log also shows only TUI markdown warnings after the failed completion:

```text
.spur/logs/spur.log.2026-05-28:3369-3372
2026-05-28T09:22:52Z WARN tui_markdown: Could not find syntax for code block
```

Those warnings are after the `09:22:11` completion and before the `09:24:57` user prompt, but they are renderer warnings, not continuation-delivery diagnostics.

## 4. Code Path: Producer to Consumer

### 4.1 Delegation Dispatch Creates a Detached Completion Handle

`handle_delegate_to_worker` creates a `DelegationRequest` with a oneshot response channel:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:458-476
graph://symbol/0f25fde07ccd2a76
```

If the inline wait expires, it spawns `spawn_result_collector` with `DetachedCompletionHandle` and returns `continuation_will_fire=true`:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:579-625
graph://symbol/0f25fde07ccd2a76
```

The comments explicitly state that the slow path hands the receiver to `spawn_result_collector`, and that the continuation bridge is the sole delivery channel:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:490-496
graph://symbol/0f25fde07ccd2a76
```

### 4.2 Delegation Terminal Event Is Emitted Separately

The terminal helper `finalize` centralizes the `DelegationCompleted` invariant:

```text
crates/spur-core/src/orchestrator/delegation/finalize.rs:3-16
```

`flush_then_emit_completed` drains worker-MCP audit state and emits `DelegationCompleted` into the funnel:

```text
crates/spur-core/src/orchestrator/delegation/finalize.rs:80-109
```

This explains why `DelegationCompleted` appearing in ndjson proves the worker terminal event reached the event funnel, but does not prove the model-visible continuation reached `run_interactive`.

### 4.3 The Collector Builds a BrainContinuation After the Oneshot Resolves

`spawn_result_collector` awaits the oneshot result or cancellation:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:25-68
graph://symbol/d52f0b580b7ce9ba
```

For detached completions, it builds the continuation:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:98-136
graph://symbol/d52f0b580b7ce9ba
```

Then it calls the injected callback:

```text
crates/spur-mcp/src/server/handlers/delegation.rs:137-141
graph://symbol/d52f0b580b7ce9ba
```

`build_detached_continuation` is a wrapper around the materializer:

```text
crates/spur-mcp/src/server/types.rs:522-542
graph://symbol/8e69e56b473ddf4d
```

### 4.4 The Callback Is Wired to Core's mpsc Ingress

`Orchestrator::set_continuation_tx` stores an `mpsc::Sender<InteractiveInput>` and the overflow deque:

```text
crates/spur-core/src/orchestrator/support.rs:43-50
graph://symbol/ba081507f8188dc1
```

`Orchestrator::build_continuation_ctx` wires `DetachedContinuationCtx::on_complete` to `report_detached_completion`:

```text
crates/spur-core/src/orchestrator/support.rs:79-118
graph://symbol/a0e12e98a8e36942
```

The same function says a missing `continuation_tx` makes the callback a no-op:

```text
crates/spur-core/src/orchestrator/support.rs:85-87
graph://symbol/a0e12e98a8e36942
```

In this incident the brain was already interactive, so the no-op path is unlikely; `InteractiveFrontendHost::spawn` creates the user channel with capacity 32 and calls `set_continuation_tx` before spawning `run_interactive`:

```text
crates/spur-interactive/src/host.rs:171-188
```

### 4.5 `report_detached_completion` Uses `try_send`, Not `send`

`report_detached_completion` attempts a non-blocking send:

```text
crates/spur-core/src/continuation_bridge.rs:41-62
graph://symbol/80fb58ee3e53727e
```

The key behavior is:

- `Ok(())`: the continuation is enqueued on the `mpsc` ingress.
- `TrySendError::Full(_)`: the continuation is pushed into `OverflowBuf`.
- `TrySendError::Closed(_)`: a warning logs `channel_closed`.

Code evidence:

```text
crates/spur-core/src/continuation_bridge.rs:63-91
graph://symbol/80fb58ee3e53727e
```

This is the first code point where a detached continuation can become invisible to the idle receiver without being lost permanently: overflow storage is durable only in memory and does not wake `run_interactive`.

### 4.6 `run_interactive` Is the Continuation Consumer

`run_interactive` owns `mut user_input_rx: mpsc::Receiver<InteractiveInput>`:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:129-136
graph://symbol/d1e4d3f22a5a6fe4
```

At the top of each loop iteration it drains the overflow deque into the scheduler:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:181-188
graph://symbol/d1e4d3f22a5a6fe4
```

Then it calls `scheduler.next`:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:190-192
graph://symbol/d1e4d3f22a5a6fe4
```

When idle, it awaits `user_input_rx.recv()`:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:194-205
graph://symbol/d1e4d3f22a5a6fe4
```

If that raw input is a `SystemContinuation`, it calls `scheduler.push_continuation` and continues to the next loop iteration:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:265-269
graph://symbol/d1e4d3f22a5a6fe4
```

During an active stream, the same receiver arm pushes `SystemContinuation` directly into the scheduler:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:1477-1504
graph://symbol/d1e4d3f22a5a6fe4
```

### 4.7 Scheduler Queues, Then Auto-Fires

`BrainScheduler::push_continuation` enforces session matching, delivered-id dedup, pending dedup, then pushes into `pending_continuations`:

```text
crates/spur-core/src/scheduler.rs:131-197
graph://symbol/39be6ec0df7b74e2
```

`BrainScheduler::next` is the core scheduling policy:

```text
crates/spur-core/src/scheduler.rs:473-502
graph://symbol/57762ddfbe537228
```

The important order:

1. If a turn is in flight, stay idle.
2. If user input is pending and no continuation is pending, return `UserPrompt`.
3. If user input and continuation are both pending, return `MergedPrompt`.
4. If only continuation is pending, return `ContinuationPrompt`.

The prompt-dispatch code converts `ContinuationPrompt` into an autonomous continuation turn:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:1209-1217
graph://symbol/d1e4d3f22a5a6fe4
```

Then it emits `PromptDispatched` with `turn_kind` and `continuations_count` before calling the ACP transport:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:1298-1330
graph://symbol/d1e4d3f22a5a6fe4
```

## 5. Subscriber Classification

The continuation subscriber is:

```text
tokio::sync::mpsc::Receiver<InteractiveInput>
```

owned by:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:129-136
graph://symbol/d1e4d3f22a5a6fe4
```

The producer is:

```text
tokio::sync::mpsc::Sender<InteractiveInput>
```

stored on `Orchestrator`:

```text
crates/spur-core/src/orchestrator.rs:156-162
```

and set via:

```text
crates/spur-core/src/orchestrator/support.rs:43-50
graph://symbol/ba081507f8188dc1
```

The event broadcast exists, but it is not the continuation injection path. It is event fanout:

```text
crates/spur-core/src/event_funnel.rs:1-10
crates/spur-core/src/event_funnel.rs:105-118
```

The durable ndjson writer is a broadcast subscriber:

```text
crates/spur-core/src/event_sink.rs:37-80
```

That sink logs `event_sink: broadcast lagged` if it misses events:

```text
crates/spur-core/src/event_sink.rs:72-75
```

No such log line was found for the incident window.

## 6. Auto-Fire Chain and the `IssuesLoaded` Side Effect

The successful event sequence was:

```text
DelegationCompleted -> IssuesLoaded -> GraphAlertsSummary -> PromptDispatched(continuation_only)
```

There is no code path where `DelegationCompleted` itself calls `refresh_pm_state`.

The refresh code is `refresh_pm_state`:

```text
crates/spur-core/src/orchestrator/pm_bridge.rs:98-158
graph://symbol/2e651695a70a8ab4
```

It emits `IssuesLoaded` after `pm.list_issues`:

```text
crates/spur-core/src/orchestrator/pm_bridge.rs:119-133
graph://symbol/2e651695a70a8ab4
```

It emits `GraphAlertsSummary` via `emit_alerts_from_report`:

```text
crates/spur-core/src/orchestrator/pm_bridge.rs:21-40
```

The interactive loop calls `refresh_pm_state` in only two visible places:

- startup:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:153-156
graph://symbol/d1e4d3f22a5a6fe4
```

- explicit `InteractiveInput::RefreshIssues`:

```text
crates/spur-core/src/orchestrator/interactive_loop.rs:757-760
graph://symbol/d1e4d3f22a5a6fe4
```

The successful `IssuesLoaded` at seq 1981 therefore appears to be a concurrently queued refresh input, not a direct side effect of `DelegationCompleted`. The TUI has explicit refresh actions that send `UserInput::RefreshIssues`:

```text
crates/spur-tui/src/app/action_routing/pm_actions.rs:6-11
crates/spur-cli/src/main.rs:1513
```

The failed case lacks the `IssuesLoaded`/`GraphAlertsSummary` pair before its next prompt, which means no refresh input woke the idle loop after `DelegationCompleted`. That difference is consistent with the overflow-latency hypothesis: the successful case had another input/event-driven refresh that caused the loop to re-enter and drain/dispatch the continuation; the failed case did not.

## 7. Failure Mode Classification

### Candidate A: Subscriber starvation / `RecvError::Lagged` on bounded broadcast channel

Status: **falsified for continuation injection**.

Evidence:

- The continuation injection subscriber is an `mpsc::Receiver<InteractiveInput>`, not `broadcast::Receiver<SpurEvent>`: `crates/spur-core/src/orchestrator/interactive_loop.rs:129-136`, `graph://symbol/d1e4d3f22a5a6fe4`.
- `report_detached_completion` sends `InteractiveInput::SystemContinuation` over `mpsc::Sender`, not event broadcast: `crates/spur-core/src/continuation_bridge.rs:41-62`, `graph://symbol/80fb58ee3e53727e`.
- The broadcast event sink can log lag: `crates/spur-core/src/event_sink.rs:72-75`; no matching lag log was found around the incident.

### Candidate B: Idle-state guard in continuation handler

Status: **falsified as written**.

Evidence:

- `BrainScheduler::next` explicitly fires `ContinuationPrompt` when idle and pending continuations exist: `crates/spur-core/src/scheduler.rs:491-502`, `graph://symbol/57762ddfbe537228`.
- The regression test symbol `next_fires_continuation_only_when_idle_and_no_user_pending` exists at `crates/spur-core/src/scheduler.rs:966-975`, `graph://symbol/9c8ae42c40437b09`.

Idle itself is not a guard. The problem is that overflowed continuations do not wake the idle loop.

### Candidate C: In-memory registry of pending delegations lost across brain idle/wake transitions

Status: **not supported by current evidence**.

Evidence:

- The production slow path does not rely on `completed_delegations` for `BlockTimeout`; `spawn_result_collector` explicitly skips map writes for that source kind: `crates/spur-mcp/src/server/handlers/delegation.rs:71-96`, `graph://symbol/d52f0b580b7ce9ba`.
- The collector owns the oneshot receiver handed off at detach time: `crates/spur-mcp/src/server/handlers/delegation.rs:586-600`, `graph://symbol/0f25fde07ccd2a76`.

There is still an in-memory dependency on the collector task and the injected callback, but no event/log evidence shows a registry reset or session re-init at the failure boundary.

### Candidate D: Race / ordering: producer wrote `DelegationCompleted` but consumer awaited a different channel

Status: **partially supported, but more precise as Candidate E**.

Evidence:

- `DelegationCompleted` is emitted to the event funnel: `crates/spur-core/src/orchestrator/delegation/finalize.rs:106-109`.
- The continuation is delivered via a separate mpsc channel: `crates/spur-core/src/continuation_bridge.rs:59-62`, `graph://symbol/80fb58ee3e53727e`.
- The consumer waits on that mpsc channel when idle: `crates/spur-core/src/orchestrator/interactive_loop.rs:194-205`, `graph://symbol/d1e4d3f22a5a6fe4`.

The event reaching ndjson does not causally imply the mpsc continuation reached the scheduler.

### Candidate E: Overflow-latency / idle-loop wakeup gap on bounded mpsc continuation ingress

Status: **best evidence-backed root cause**.

Evidence:

- The ingress channel is bounded: `crates/spur-interactive/src/host.rs:171-188` creates `mpsc::channel::<InteractiveInput>(32)` and wires it via `set_continuation_tx`.
- The continuation producer uses `try_send`, not awaited `send`: `crates/spur-core/src/continuation_bridge.rs:59-62`, `graph://symbol/80fb58ee3e53727e`.
- On `TrySendError::Full`, it pushes into `OverflowBuf`: `crates/spur-core/src/continuation_bridge.rs:72-82`, `graph://symbol/80fb58ee3e53727e`.
- `OverflowBuf` is drained only at the top of the `run_interactive` loop: `crates/spur-core/src/orchestrator/interactive_loop.rs:181-188`, `graph://symbol/d1e4d3f22a5a6fe4`.
- If the loop is idle in `user_input_rx.recv()` and the continuation went to overflow instead of the channel, no wake happens.
- When the next user input arrives, the loop handles raw `Message` by `scheduler.push_user(raw)` and continues: `crates/spur-core/src/orchestrator/interactive_loop.rs:270-274`, `graph://symbol/d1e4d3f22a5a6fe4`.
- `BrainScheduler::next` gives a pending user prompt precedence if no continuation was drained yet; if overflow draining happens on the following top-of-loop after the user prompt path, the event at seq 3037 can be `user_only`: `crates/spur-core/src/scheduler.rs:481-489`, `graph://symbol/57762ddfbe537228`.

The one missing direct proof is the `A_report_detached_completion outcome=overflow_pushed` debug log. It was not visible in the active log, likely because the debug target was disabled. A focused test can prove or disprove this exact sequence.

## 8. Sequence Diagram

```mermaid
sequenceDiagram
    participant Brain as Brain model/tool call
    participant MCP as spur-mcp handle_delegate_to_worker
    participant Worker as execute_delegation worker
    participant Funnel as event_funnel/event_sink
    participant Collector as spawn_result_collector
    participant Bridge as report_detached_completion
    participant Ingress as mpsc<InteractiveInput>
    participant Overflow as OverflowBuf
    participant Loop as run_interactive
    participant Scheduler as BrainScheduler

    Brain->>MCP: delegate_to_worker
    MCP->>Worker: DelegationRequest + oneshot
    MCP->>Collector: spawn_result_collector after inline timeout
    MCP-->>Brain: continuation_will_fire=true
    Worker->>Funnel: DelegationCompleted
    Funnel-->>Funnel: seq stamped and ndjson written
    Worker->>Collector: oneshot DelegationResult
    Collector->>Bridge: on_complete(BrainContinuation)
    Bridge->>Ingress: try_send(SystemContinuation)
    alt ingress has capacity
        Ingress->>Loop: wake recv()
        Loop->>Scheduler: push_continuation
        Scheduler-->>Loop: ContinuationPrompt
        Loop->>Funnel: PromptDispatched continuation_only
    else ingress full
        Bridge->>Overflow: push_back(continuation)
        Note over Overflow,Loop: No wake; overflow drains only when loop reaches top again
        Brain->>Ingress: later user input
        Ingress->>Loop: wake recv() with Message
        Loop->>Scheduler: push_user
        Scheduler-->>Loop: UserPrompt
        Loop->>Funnel: PromptDispatched user_only
    end
```

## 9. Additional Test to Disambiguate

Add a deterministic test that fills the interactive ingress channel to capacity, calls `report_detached_completion`, asserts the continuation lands in `OverflowBuf`, and then drives `run_interactive` while it is idle. The expected current failure is that no continuation-only dispatch happens until some unrelated input wakes the loop. That test should be placed near existing continuation bridge / scheduler integration tests, for example `crates/spur-core/tests/continuation_integration.rs`, with a fake brain connection that records `PromptDispatched`.

Also add info-level probes for:

- `A_report_detached_completion outcome=try_send_ok`
- `A_report_detached_completion outcome=overflow_pushed`
- `B_push_continuation outcome=enqueued`
- `D_prompt_dispatch turn_kind=...`

The current debug probes exist but were not sufficient in production logs.

## 10. Fix Sketch

Surgical fix: in `crates/spur-core/src/continuation_bridge.rs:41-94` (`report_detached_completion`, `graph://symbol/80fb58ee3e53727e`), replace the `try_send`/overflow-first behavior with an awaitable `continuation_tx.send(InteractiveInput::SystemContinuation { ... }).await` for detached completions, retaining overflow only as a shutdown/fallback path if `send` returns `Closed` or if a bounded timeout is deliberately exceeded. This makes a completed worker wake the idle `run_interactive` receiver instead of silently parking the continuation in an overflow deque that is only drained on a later loop iteration. If blocking the collector is unacceptable, spawn a small delivery task that awaits `send` and logs a WARN if delivery exceeds a short threshold; do not push to overflow without also waking the loop.

## 11. Conclusion

The root cause is **(e) overflow-latency / idle-loop wakeup gap on bounded mpsc continuation ingress**, not broadcast lag. The durable `DelegationCompleted` event and the model-visible continuation are separate channels. The durable event succeeded; the continuation likely took a path that did not wake the idle interactive loop, leaving the next user turn to dispatch as `user_only`.
